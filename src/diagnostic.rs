use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_text_size::{Ranged, TextRange};

use crate::index::{
    CallableIndex, CallableSummary, ModelId, ModuleAnalysis, RelationDirection, WorkspaceIndex,
    apply_import_statement, qualify_expr,
};

pub const MISSING_EAGER_LOAD: &str = "DJ001";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrmDiagnostic {
    pub code: &'static str,
    pub range: TextRange,
    pub message: String,
    pub method: &'static str,
    pub relation_path: String,
}

struct PendingDiagnostic {
    diagnostic: OrmDiagnostic,
    iteration_range: TextRange,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FetchMode {
    #[default]
    One,
    Peers,
    Raise,
    Unknown,
}

#[derive(Debug, Clone, Default)]
struct LoadingState {
    selected: HashSet<String>,
    prefetched: HashSet<String>,
    select_all: bool,
    selected_unknown: bool,
    prefetched_unknown: bool,
    fetch_mode: FetchMode,
}

#[derive(Debug, Clone)]
struct QueryState {
    model: ModelId,
    root_model: ModelId,
    relation_prefix: Vec<String>,
    prefix_all_selectable: bool,
    loading: LoadingState,
    local_loading: Option<LoadingState>,
    iteration_range: Option<TextRange>,
}

impl QueryState {
    fn new(model: ModelId) -> Self {
        Self {
            root_model: model.clone(),
            model,
            relation_prefix: Vec::new(),
            prefix_all_selectable: true,
            loading: LoadingState::default(),
            local_loading: None,
            iteration_range: None,
        }
    }

    fn compose_relation_path(&self, relation_path: String, all_selectable: bool) -> (String, bool) {
        if self.relation_prefix.is_empty() {
            return (relation_path, all_selectable);
        }
        (
            format!("{}__{relation_path}", self.relation_prefix.join("__")),
            self.prefix_all_selectable && all_selectable,
        )
    }

    fn local_relation_is_loaded(&self, path: &str, all_selectable: bool) -> bool {
        self.local_loading
            .as_ref()
            .is_some_and(|loading| loading_state_covers(loading, path, all_selectable))
    }

    fn active_loading_mut(&mut self) -> &mut LoadingState {
        self.local_loading.as_mut().unwrap_or(&mut self.loading)
    }
}

#[derive(Debug, Clone, Default)]
struct Scope {
    imports: HashMap<String, String>,
    queries: HashMap<String, QueryState>,
    repeated_items: HashMap<String, QueryState>,
    model_instances: HashMap<String, QueryState>,
    class_model: Option<ModelId>,
    class_instance_state: Option<QueryState>,
}

pub fn analyze_diagnostics(
    index: &WorkspaceIndex,
    callables: &CallableIndex,
    analysis: &ModuleAnalysis,
) -> Vec<OrmDiagnostic> {
    let mut analyzer = Analyzer {
        index,
        callables,
        analysis,
        diagnostics: Vec::new(),
        seen: HashSet::new(),
    };
    analyzer.analyze_body(
        &analysis.body,
        &mut Scope {
            imports: analysis.imports.clone(),
            ..Scope::default()
        },
    );
    analyzer.diagnostics.sort_by_key(|pending| {
        let diagnostic = &pending.diagnostic;
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.relation_path.clone(),
        )
    });
    analyzer
        .diagnostics
        .into_iter()
        .map(|pending| pending.diagnostic)
        .collect()
}

struct Analyzer<'a> {
    index: &'a WorkspaceIndex,
    callables: &'a CallableIndex,
    analysis: &'a ModuleAnalysis,
    diagnostics: Vec<PendingDiagnostic>,
    seen: HashSet<(TextRange, String)>,
}

impl Analyzer<'_> {
    fn analyze_body(&mut self, body: &[Stmt], scope: &mut Scope) {
        for statement in body {
            self.analyze_statement(statement, scope);
        }
    }

    fn analyze_statement(&mut self, statement: &Stmt, scope: &mut Scope) {
        match statement {
            Stmt::FunctionDef(function) => {
                let mut function_scope = Scope {
                    imports: scope.imports.clone(),
                    ..Scope::default()
                };
                self.bind_function_parameters(function, scope, &mut function_scope);
                self.analyze_body(&function.body, &mut function_scope);
            }
            Stmt::ClassDef(class) => {
                let class_instance_state = self.resolve_admin_queryset_state(class, scope);
                let mut class_scope = Scope {
                    imports: scope.imports.clone(),
                    class_model: self
                        .index
                        .resolve_model_symbol(&self.analysis.module_name, class.name.as_str()),
                    class_instance_state,
                    ..Scope::default()
                };
                self.analyze_body(&class.body, &mut class_scope);
            }
            Stmt::Assign(assign) => {
                self.analyze_expr(&assign.value, scope);
                if assign.targets.len() == 1 {
                    self.update_binding(&assign.targets[0], &assign.value, scope);
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.analyze_expr(value, scope);
                    self.update_binding(&assign.target, value, scope);
                }
                if let Expr::Name(name) = assign.target.as_ref()
                    && !scope.queries.contains_key(name.id.as_str())
                    && let Some(model) = self.resolve_annotation_model(&assign.annotation, scope)
                {
                    scope
                        .model_instances
                        .insert(name.id.to_string(), QueryState::new(model));
                }
            }
            Stmt::AugAssign(assign) => {
                self.analyze_expr(&assign.target, scope);
                self.analyze_expr(&assign.value, scope);
                self.remove_binding(&assign.target, scope);
            }
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.remove_binding(target, scope);
                }
            }
            Stmt::For(for_statement) => {
                self.analyze_expr(&for_statement.iter, scope);
                let query = self.resolve_query_state(&for_statement.iter, scope);
                let mut body_scope = scope.clone();
                self.bind_repeated_target(
                    &for_statement.target,
                    query,
                    for_statement.iter.range(),
                    &mut body_scope,
                );
                self.analyze_body(&for_statement.body, &mut body_scope);
                let mut else_scope = scope.clone();
                self.analyze_body(&for_statement.orelse, &mut else_scope);
            }
            Stmt::While(while_statement) => {
                self.analyze_expr(&while_statement.test, scope);
                let mut body_scope = scope.clone();
                self.analyze_body(&while_statement.body, &mut body_scope);
                let mut else_scope = scope.clone();
                self.analyze_body(&while_statement.orelse, &mut else_scope);
            }
            Stmt::If(if_statement) => {
                self.analyze_expr(&if_statement.test, scope);
                let mut body_scope = scope.clone();
                self.analyze_body(&if_statement.body, &mut body_scope);
                for clause in &if_statement.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.analyze_expr(test, scope);
                    }
                    let mut clause_scope = scope.clone();
                    self.analyze_body(&clause.body, &mut clause_scope);
                }
            }
            Stmt::With(with_statement) => {
                for item in &with_statement.items {
                    self.analyze_expr(&item.context_expr, scope);
                }
                let mut body_scope = scope.clone();
                self.analyze_body(&with_statement.body, &mut body_scope);
            }
            Stmt::Try(try_statement) => {
                let mut body_scope = scope.clone();
                self.analyze_body(&try_statement.body, &mut body_scope);
                for handler in &try_statement.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    let mut handler_scope = scope.clone();
                    self.analyze_body(&handler.body, &mut handler_scope);
                }
                let mut else_scope = scope.clone();
                self.analyze_body(&try_statement.orelse, &mut else_scope);
                let mut finally_scope = scope.clone();
                self.analyze_body(&try_statement.finalbody, &mut finally_scope);
            }
            Stmt::Match(match_statement) => {
                self.analyze_expr(&match_statement.subject, scope);
                for case in &match_statement.cases {
                    if let Some(guard) = &case.guard {
                        self.analyze_expr(guard, scope);
                    }
                    let mut case_scope = scope.clone();
                    self.analyze_body(&case.body, &mut case_scope);
                }
            }
            Stmt::Return(return_statement) => {
                if let Some(value) = &return_statement.value {
                    self.analyze_expr(value, scope);
                }
            }
            Stmt::Raise(raise) => {
                if let Some(exception) = &raise.exc {
                    self.analyze_expr(exception, scope);
                }
                if let Some(cause) = &raise.cause {
                    self.analyze_expr(cause, scope);
                }
            }
            Stmt::Assert(assertion) => {
                self.analyze_expr(&assertion.test, scope);
                if let Some(message) = &assertion.msg {
                    self.analyze_expr(message, scope);
                }
            }
            Stmt::Expr(expression) => {
                self.analyze_expr(&expression.value, scope);
                self.apply_prefetch_related_objects(&expression.value, scope);
            }
            Stmt::TypeAlias(alias) => self.analyze_expr(&alias.value, scope),
            Stmt::Import(_) | Stmt::ImportFrom(_) => apply_import_statement(
                statement,
                &self.analysis.module_name,
                self.analysis.is_package,
                &mut scope.imports,
            ),
            Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    fn analyze_expr(&mut self, expression: &Expr, scope: &Scope) {
        ExpressionAnalyzer {
            analyzer: self,
            scope,
        }
        .visit_expr(expression);
    }

    fn analyze_comprehension(
        &mut self,
        generators: &[ast::Comprehension],
        outputs: &[&Expr],
        scope: &Scope,
    ) {
        let mut comprehension_scope = scope.clone();
        for generator in generators {
            self.analyze_expr(&generator.iter, &comprehension_scope);
            let query = self.resolve_query_state(&generator.iter, &comprehension_scope);
            self.bind_repeated_target(
                &generator.target,
                query,
                generator.range,
                &mut comprehension_scope,
            );
            for condition in &generator.ifs {
                self.analyze_expr(condition, &comprehension_scope);
            }
        }
        for output in outputs {
            self.analyze_expr(output, &comprehension_scope);
        }
    }

    fn inspect_attribute_chain(&mut self, expression: &Expr, scope: &Scope) -> bool {
        let Some((root, segments)) = attribute_chain(expression) else {
            return false;
        };
        if segments
            .last()
            .is_some_and(|(segment, _)| relation_write_method(segment))
        {
            return true;
        }
        let Some(query) = scope.repeated_items.get(root) else {
            return false;
        };
        if self.inspect_property_access(query, &segments) {
            return true;
        }
        let Some((relation_path, all_selectable, relation_count)) =
            self.relation_details(&query.model, segments.iter().map(|(segment, _)| *segment))
        else {
            return true;
        };
        let range = segments[relation_count - 1].1;
        if query.local_relation_is_loaded(&relation_path, all_selectable) {
            return true;
        }
        let (relation_path, all_selectable) =
            query.compose_relation_path(relation_path, all_selectable);
        self.emit_missing_eager_load(query, relation_path, all_selectable, range);
        true
    }

    fn inspect_property_access(
        &mut self,
        query: &QueryState,
        segments: &[(&str, TextRange)],
    ) -> bool {
        let Some(mut model) = self.index.model(&query.model) else {
            return false;
        };
        let mut prefix = Vec::new();
        for (segment, range) in segments {
            if let Some(field) = model.relation_for_accessor(segment)
                && let Some(related_model) = field
                    .related_model
                    .as_ref()
                    .and_then(|model_id| self.index.model(model_id))
            {
                prefix.push(*segment);
                model = related_model;
                continue;
            }

            let Some(summary) = self.callables.property_summary(&model.id, segment) else {
                return false;
            };
            let paths = summary.paths.clone();
            let mut emitted = false;
            for path in paths.iter().filter(|path| path.parameter_index == 0) {
                let mut relation_segments = prefix.clone();
                relation_segments.extend(path.segments.iter().map(String::as_str));
                let Some((relation_path, all_selectable, _)) =
                    self.relation_details(&query.model, relation_segments)
                else {
                    continue;
                };
                if query.local_relation_is_loaded(&relation_path, all_selectable) {
                    emitted = true;
                    continue;
                }
                let (relation_path, all_selectable) =
                    query.compose_relation_path(relation_path, all_selectable);
                self.emit_missing_eager_load(query, relation_path, all_selectable, *range);
                emitted = true;
            }
            return emitted;
        }
        false
    }

    fn relation_details<'segment>(
        &self,
        model_id: &ModelId,
        segments: impl IntoIterator<Item = &'segment str>,
    ) -> Option<(String, bool, usize)> {
        let mut model = self.index.model(model_id)?;
        let mut relation_path = Vec::new();
        let mut all_selectable = true;
        for segment in segments {
            let Some(field) = model.relation_for_accessor(segment) else {
                break;
            };
            relation_path.push(segment);
            all_selectable &= field.supports_select_related();
            let Some(related_model) = field
                .related_model
                .as_ref()
                .and_then(|model_id| self.index.model(model_id))
            else {
                break;
            };
            model = related_model;
        }
        (!relation_path.is_empty()).then(|| {
            let relation_count = relation_path.len();
            (relation_path.join("__"), all_selectable, relation_count)
        })
    }

    fn emit_missing_eager_load(
        &mut self,
        query: &QueryState,
        relation_path: String,
        all_selectable: bool,
        range: TextRange,
    ) {
        if relation_is_loaded(query, &relation_path, all_selectable)
            || self
                .analysis
                .suppresses_diagnostic(MISSING_EAGER_LOAD, range.start())
        {
            return;
        }

        let method = if all_selectable {
            "select_related"
        } else {
            "prefetch_related"
        };
        let iteration_range = query.iteration_range.unwrap_or(range);
        let nested_prefix = format!("{relation_path}__");
        if self.seen.iter().any(|(seen_iteration, seen_path)| {
            *seen_iteration == iteration_range && seen_path.starts_with(&nested_prefix)
        }) {
            return;
        }
        let parent_paths = self
            .seen
            .iter()
            .filter(|(seen_iteration, seen_path)| {
                *seen_iteration == iteration_range
                    && relation_path.starts_with(&format!("{seen_path}__"))
            })
            .map(|(_, path)| path.clone())
            .collect::<HashSet<_>>();
        if !parent_paths.is_empty() {
            self.seen.retain(|(seen_iteration, seen_path)| {
                *seen_iteration != iteration_range || !parent_paths.contains(seen_path)
            });
            self.diagnostics.retain(|pending| {
                pending.iteration_range != iteration_range
                    || !parent_paths.contains(&pending.diagnostic.relation_path)
            });
        }
        if !self.seen.insert((iteration_range, relation_path.clone())) {
            return;
        }
        self.diagnostics.push(PendingDiagnostic {
            diagnostic: OrmDiagnostic {
                code: MISSING_EAGER_LOAD,
                range: if query.relation_prefix.is_empty() {
                    range
                } else {
                    iteration_range
                },
                message: format!(
                    "Accessing `{relation_path}` for each `{}` may issue an extra query per row; add `{method}(\"{relation_path}\")` to the QuerySet.",
                    self.index
                        .model(&query.root_model)
                        .map_or(query.root_model.as_str(), |model| model.class_name.as_str())
                ),
                method,
                relation_path,
            },
            iteration_range,
        });
    }

    fn inspect_callable_call(&mut self, call: &ast::ExprCall, scope: &Scope) {
        let Some((summary, receiver)) = self.resolve_callable_summary(call, scope) else {
            return;
        };
        let summary = summary.clone();
        for path in &summary.paths {
            let argument = if path.parameter_index < summary.bound_parameter_count {
                (path.parameter_index == 0).then_some(receiver).flatten()
            } else {
                let positional_index = path.parameter_index - summary.bound_parameter_count;
                call.arguments.args.get(positional_index).or_else(|| {
                    let parameter_name = summary.parameters.get(path.parameter_index)?;
                    call.arguments.keywords.iter().find_map(|keyword| {
                        (keyword.arg.as_ref()?.as_str() == parameter_name).then_some(&keyword.value)
                    })
                })
            };
            let Some(argument) = argument else {
                continue;
            };
            let Some((query, mut prefix)) = repeated_expression_origin(argument, scope) else {
                continue;
            };
            prefix.extend(path.segments.iter().map(String::as_str));
            let Some((relation_path, all_selectable, _)) =
                self.relation_details(&query.model, prefix)
            else {
                continue;
            };
            if query.local_relation_is_loaded(&relation_path, all_selectable) {
                continue;
            }
            let (relation_path, all_selectable) =
                query.compose_relation_path(relation_path, all_selectable);
            self.emit_missing_eager_load(query, relation_path, all_selectable, argument.range());
        }
    }

    fn resolve_callable_summary<'call>(
        &self,
        call: &'call ast::ExprCall,
        scope: &Scope,
    ) -> Option<(&CallableSummary, Option<&'call Expr>)> {
        if let Some(qualified) = qualify_expr(
            &call.func,
            &self.analysis.module_name,
            &self.analysis.local_class_names,
            &scope.imports,
        ) {
            if let Some(summary) = self.callables.summary(&qualified) {
                return Some((summary, None));
            }
            if !qualified.contains('.')
                && let Some(summary) = self
                    .callables
                    .summary(&format!("{}.{}", self.analysis.module_name, qualified))
            {
                return Some((summary, None));
            }
        }

        let Expr::Attribute(method) = call.func.as_ref() else {
            return None;
        };
        let (query, _) = repeated_expression_origin(&method.value, scope)?;
        let summary = self
            .callables
            .summary(&format!("{}.{}", query.model, method.attr))?;
        Some((summary, Some(&method.value)))
    }

    fn update_binding(&self, target: &Expr, value: &Expr, scope: &mut Scope) {
        let Expr::Name(name) = target else {
            return;
        };
        if let Some(query) = self.resolve_query_state(value, scope) {
            scope.queries.insert(name.id.to_string(), query);
        } else {
            scope.queries.remove(name.id.as_str());
        }
        scope.repeated_items.remove(name.id.as_str());
        scope.model_instances.remove(name.id.as_str());
    }

    fn remove_binding(&self, target: &Expr, scope: &mut Scope) {
        if let Expr::Name(name) = target {
            scope.queries.remove(name.id.as_str());
            scope.repeated_items.remove(name.id.as_str());
            scope.model_instances.remove(name.id.as_str());
        }
    }

    fn bind_repeated_target(
        &self,
        target: &Expr,
        query: Option<QueryState>,
        iteration_range: TextRange,
        scope: &mut Scope,
    ) {
        let Expr::Name(name) = target else {
            return;
        };
        scope.queries.remove(name.id.as_str());
        scope.model_instances.remove(name.id.as_str());
        if let Some(mut query) = query {
            query.iteration_range.get_or_insert(iteration_range);
            scope.repeated_items.insert(name.id.to_string(), query);
        } else {
            scope.repeated_items.remove(name.id.as_str());
        }
    }

    fn apply_prefetch_related_objects(&self, expression: &Expr, scope: &mut Scope) {
        let Expr::Call(call) = expression else {
            return;
        };
        let Some(qualified) = qualify_expr(
            &call.func,
            &self.analysis.module_name,
            &self.analysis.local_class_names,
            &scope.imports,
        ) else {
            return;
        };
        if qualified != "django.db.models.prefetch_related_objects"
            && qualified != "django.db.models.query.prefetch_related_objects"
        {
            return;
        }

        let Some(Expr::Name(query_name)) = call.arguments.args.first() else {
            return;
        };
        let Some(query) = scope.queries.get_mut(query_name.id.as_str()) else {
            return;
        };
        let (paths, unknown) = literal_relation_paths_from_arguments(
            &call.arguments.args[1..],
            !call.arguments.keywords.is_empty(),
        );
        let loading = query.active_loading_mut();
        loading.prefetched.extend(paths);
        loading.prefetched_unknown |= unknown;
    }

    fn resolve_query_state(&self, expression: &Expr, scope: &Scope) -> Option<QueryState> {
        match expression {
            Expr::Name(name) => scope.queries.get(name.id.as_str()).cloned(),
            Expr::Subscript(subscript) if matches!(subscript.slice.as_ref(), Expr::Slice(_)) => {
                self.resolve_query_state(&subscript.value, scope)
            }
            Expr::Attribute(attribute) if attribute.attr.as_str() == "objects" => {
                let model = self.resolve_model_reference(&attribute.value, scope)?;
                Some(QueryState::new(model))
            }
            Expr::Attribute(_) => self.resolve_related_manager(expression, scope),
            Expr::Call(call) => {
                if let Expr::Name(function) = call.func.as_ref()
                    && matches!(
                        function.id.as_str(),
                        "list" | "tuple" | "set" | "iter" | "reversed"
                    )
                    && call.arguments.args.len() == 1
                    && call.arguments.keywords.is_empty()
                {
                    return self.resolve_query_state(&call.arguments.args[0], scope);
                }
                if let Some((summary, _)) = self.resolve_callable_summary(call, scope)
                    && let Some(return_model) = &summary.return_collection_model
                    && let Some(model) = self.index.resolve_qualified_model(return_model)
                {
                    let mut query = QueryState::new(model);
                    query
                        .loading
                        .selected
                        .extend(summary.return_selected.iter().cloned());
                    query
                        .loading
                        .prefetched
                        .extend(summary.return_prefetched.iter().cloned());
                    query.loading.select_all = summary.return_select_all;
                    query.loading.selected_unknown = summary.return_selected_unknown;
                    query.loading.prefetched_unknown = summary.return_prefetched_unknown;
                    return Some(query);
                }
                let Expr::Attribute(method) = call.func.as_ref() else {
                    return None;
                };
                let mut query = self
                    .resolve_query_state(&method.value, scope)
                    .or_else(|| self.resolve_related_manager(&method.value, scope))?;
                match method.attr.as_str() {
                    "all" | "filter" | "exclude" | "order_by" | "distinct" | "only" | "defer"
                    | "annotate" | "iterator" | "aiterator" => {}
                    "select_related" => {
                        let loading = query.active_loading_mut();
                        if call.arguments.args.is_empty() {
                            loading.select_all = true;
                        } else if call
                            .arguments
                            .args
                            .first()
                            .is_some_and(|argument| matches!(argument, Expr::NoneLiteral(_)))
                        {
                            loading.selected.clear();
                            loading.select_all = false;
                            loading.selected_unknown = false;
                        } else {
                            let (paths, unknown) = literal_relation_paths(call);
                            loading.selected.extend(paths);
                            loading.selected_unknown |= unknown;
                        }
                    }
                    "prefetch_related" => {
                        let loading = query.active_loading_mut();
                        if call.arguments.args.len() == 1
                            && call
                                .arguments
                                .args
                                .first()
                                .is_some_and(|argument| matches!(argument, Expr::NoneLiteral(_)))
                        {
                            loading.prefetched.clear();
                            loading.prefetched_unknown = false;
                        } else {
                            let (paths, unknown) = literal_relation_paths(call);
                            loading.prefetched.extend(paths);
                            loading.prefetched_unknown |= unknown;
                        }
                    }
                    "fetch_mode" => {
                        let fetch_mode = call
                            .arguments
                            .args
                            .first()
                            .and_then(fetch_mode_from_expression)
                            .unwrap_or(FetchMode::Unknown);
                        query.active_loading_mut().fetch_mode = fetch_mode;
                    }
                    method if queryset_method_returns_model_instances(method) => {}
                    _ => return None,
                }
                Some(query)
            }
            _ => None,
        }
    }

    fn resolve_model_reference(&self, expression: &Expr, scope: &Scope) -> Option<ModelId> {
        if let Expr::Name(name) = expression
            && let Some(model) = self
                .index
                .resolve_model_symbol(&self.analysis.module_name, name.id.as_str())
        {
            return Some(model);
        }
        let qualified = qualify_expr(
            expression,
            &self.analysis.module_name,
            &self.analysis.local_class_names,
            &scope.imports,
        )?;
        self.index.resolve_qualified_model(&qualified)
    }

    fn resolve_related_manager(&self, expression: &Expr, scope: &Scope) -> Option<QueryState> {
        let (root, segments) = attribute_chain(expression)?;
        let repeated = scope.repeated_items.get(root);
        let root_state = repeated.or_else(|| scope.model_instances.get(root))?;
        let mut model = self.index.model(&root_state.model)?;
        let mut relation_is_manager = None;
        let mut relation_direction = None;
        let mut manager_parent_model = None;
        let mut manager_path = Vec::new();
        let mut manager_all_selectable = true;
        for (segment, _) in segments {
            let field = model.relation_for_accessor(segment)?;
            let related_model = field.related_model.as_ref()?;
            manager_parent_model = Some(model.id.clone());
            relation_is_manager = Some(!field.supports_select_related());
            relation_direction = field.relation_direction;
            manager_path.push(segment.to_string());
            manager_all_selectable &= field.supports_select_related();
            model = self.index.model(related_model)?;
        }
        relation_is_manager?.then(|| {
            let cached_parent_relation = (relation_direction == Some(RelationDirection::Reverse))
                .then(|| {
                    let parent = manager_parent_model.as_ref()?;
                    let mut relations = model.fields.iter().filter(|field| {
                        field.relation_direction == Some(RelationDirection::Forward)
                            && field.related_model.as_ref() == Some(parent)
                    });
                    let relation = relations.next()?;
                    relations.next().is_none().then(|| relation.name.clone())
                })
                .flatten();
            if repeated.is_some() {
                let mut query = root_state.clone();
                query.model = model.id.clone();
                query.relation_prefix.extend(manager_path);
                query.prefix_all_selectable &= manager_all_selectable;
                query.local_loading = Some(LoadingState::default());
                if let Some(relation) = cached_parent_relation {
                    query
                        .local_loading
                        .as_mut()
                        .expect("related manager loading state")
                        .selected
                        .insert(relation);
                }
                return query;
            }

            let mut query = QueryState::new(model.id.clone());
            let prefix = format!("{}__", manager_path.join("__"));
            query.loading.prefetched.extend(
                root_state
                    .loading
                    .prefetched
                    .iter()
                    .filter_map(|path| path.strip_prefix(&prefix).map(ToOwned::to_owned)),
            );
            query.loading.prefetched_unknown = root_state.loading.prefetched_unknown;
            if let Some(relation) = cached_parent_relation {
                query.loading.selected.insert(relation);
            }
            query
        })
    }

    fn bind_function_parameters(
        &self,
        function: &ast::StmtFunctionDef,
        parent_scope: &Scope,
        function_scope: &mut Scope,
    ) {
        for parameter in function.parameters.iter() {
            if let Some(model) = parameter
                .annotation()
                .and_then(|annotation| self.resolve_annotation_model(annotation, parent_scope))
            {
                let admin_state = parent_scope
                    .class_instance_state
                    .as_ref()
                    .filter(|state| state.model == model)
                    .cloned();
                if let Some(mut state) = admin_state {
                    state.iteration_range = Some(function.range);
                    function_scope
                        .repeated_items
                        .insert(parameter.name().to_string(), state);
                } else {
                    function_scope
                        .model_instances
                        .insert(parameter.name().to_string(), QueryState::new(model));
                }
            }
        }

        if let Some(class_model) = &parent_scope.class_model
            && let Some(parameter) = function.parameters.iter().next()
            && matches!(parameter.name().as_str(), "self" | "cls")
        {
            function_scope.model_instances.insert(
                parameter.name().to_string(),
                QueryState::new(class_model.clone()),
            );
        }
    }

    fn resolve_admin_queryset_state(
        &self,
        class: &ast::StmtClassDef,
        scope: &Scope,
    ) -> Option<QueryState> {
        let model = class.decorator_list.iter().find_map(|decorator| {
            let Expr::Call(call) = &decorator.expression else {
                return None;
            };
            let qualified = qualify_expr(
                &call.func,
                &self.analysis.module_name,
                &self.analysis.local_class_names,
                &scope.imports,
            )?;
            if qualified != "django.contrib.admin.register" {
                return None;
            }
            call.arguments
                .args
                .first()
                .and_then(|model| self.resolve_model_reference(model, scope))
        })?;
        let get_queryset = class.body.iter().find_map(|statement| match statement {
            Stmt::FunctionDef(function) if function.name.as_str() == "get_queryset" => {
                Some(function)
            }
            _ => None,
        });
        let mut state = QueryState::new(model);
        if let Some(get_queryset) = get_queryset {
            let mut visitor = AdminQuerysetVisitor { state: &mut state };
            for statement in &get_queryset.body {
                visitor.visit_stmt(statement);
            }
        }
        Some(state)
    }

    fn resolve_annotation_model(&self, annotation: &Expr, scope: &Scope) -> Option<ModelId> {
        match annotation {
            Expr::Name(_) | Expr::Attribute(_) => self.resolve_model_reference(annotation, scope),
            Expr::BinOp(binary) => {
                let left = self.resolve_annotation_model(&binary.left, scope);
                let right = self.resolve_annotation_model(&binary.right, scope);
                match (left, right) {
                    (Some(left), Some(right)) if left == right => Some(left),
                    (Some(model), None) | (None, Some(model)) => Some(model),
                    _ => None,
                }
            }
            Expr::Subscript(subscript) => {
                let wrapper = qualify_expr(
                    &subscript.value,
                    &self.analysis.module_name,
                    &self.analysis.local_class_names,
                    &scope.imports,
                )?;
                matches!(
                    wrapper.rsplit('.').next(),
                    Some("Optional" | "Union" | "Annotated")
                )
                .then(|| self.resolve_annotation_model(&subscript.slice, scope))
                .flatten()
            }
            Expr::Tuple(tuple) => {
                let mut models = tuple
                    .elts
                    .iter()
                    .filter_map(|item| self.resolve_annotation_model(item, scope));
                let model = models.next()?;
                models.all(|candidate| candidate == model).then_some(model)
            }
            _ => None,
        }
    }
}

struct ExpressionAnalyzer<'a, 'b, 'scope> {
    analyzer: &'a mut Analyzer<'b>,
    scope: &'scope Scope,
}

struct AdminQuerysetVisitor<'a> {
    state: &'a mut QueryState,
}

impl Visitor<'_> for AdminQuerysetVisitor<'_> {
    fn visit_stmt(&mut self, statement: &Stmt) {
        if matches!(statement, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            return;
        }
        visitor::walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &Expr) {
        if let Expr::Call(call) = expression
            && let Expr::Attribute(method) = call.func.as_ref()
        {
            let (paths, unknown) = literal_relation_paths(call);
            match method.attr.as_str() {
                "select_related" => {
                    self.state.loading.selected.extend(paths);
                    self.state.loading.selected_unknown |= unknown;
                }
                "prefetch_related" => {
                    self.state.loading.prefetched.extend(paths);
                    self.state.loading.prefetched_unknown |= unknown;
                }
                _ => {}
            }
        }
        visitor::walk_expr(self, expression);
    }
}

impl<'ast> Visitor<'ast> for ExpressionAnalyzer<'_, '_, '_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        match expression {
            Expr::ListComp(comprehension) => self.analyzer.analyze_comprehension(
                &comprehension.generators,
                &[&comprehension.elt],
                self.scope,
            ),
            Expr::SetComp(comprehension) => self.analyzer.analyze_comprehension(
                &comprehension.generators,
                &[&comprehension.elt],
                self.scope,
            ),
            Expr::DictComp(comprehension) => {
                let mut outputs = Vec::with_capacity(2);
                if let Some(key) = &comprehension.key {
                    outputs.push(key.as_ref());
                }
                outputs.push(comprehension.value.as_ref());
                self.analyzer.analyze_comprehension(
                    &comprehension.generators,
                    &outputs,
                    self.scope,
                );
            }
            Expr::Generator(comprehension) => self.analyzer.analyze_comprehension(
                &comprehension.generators,
                &[&comprehension.elt],
                self.scope,
            ),
            Expr::Call(call) if matches!(call.func.as_ref(), Expr::Attribute(method) if relation_write_method(method.attr.as_str())) =>
            {
                for argument in &call.arguments.args {
                    self.visit_expr(argument);
                }
                for keyword in &call.arguments.keywords {
                    self.visit_expr(&keyword.value);
                }
            }
            Expr::Call(call) => {
                self.analyzer.inspect_callable_call(call, self.scope);
                visitor::walk_expr(self, expression);
            }
            Expr::Attribute(_)
                if self
                    .analyzer
                    .inspect_attribute_chain(expression, self.scope) => {}
            _ => visitor::walk_expr(self, expression),
        }
    }
}

fn attribute_chain(expression: &Expr) -> Option<(&str, Vec<(&str, TextRange)>)> {
    let mut current = expression;
    let mut segments = Vec::new();
    while let Expr::Attribute(attribute) = current {
        segments.push((attribute.attr.as_str(), attribute.attr.range));
        current = &attribute.value;
    }
    let Expr::Name(root) = current else {
        return None;
    };
    segments.reverse();
    Some((root.id.as_str(), segments))
}

fn repeated_expression_origin<'scope, 'expression>(
    expression: &'expression Expr,
    scope: &'scope Scope,
) -> Option<(&'scope QueryState, Vec<&'expression str>)> {
    let (root, segments) = attribute_chain(expression)?;
    Some((
        scope.repeated_items.get(root)?,
        segments.into_iter().map(|(segment, _)| segment).collect(),
    ))
}

fn literal_relation_paths(call: &ast::ExprCall) -> (Vec<String>, bool) {
    literal_relation_paths_from_arguments(&call.arguments.args, !call.arguments.keywords.is_empty())
}

fn queryset_method_returns_model_instances(method: &str) -> bool {
    !matches!(
        method,
        "get"
            | "aget"
            | "first"
            | "afirst"
            | "last"
            | "alast"
            | "earliest"
            | "aearliest"
            | "latest"
            | "alatest"
            | "count"
            | "acount"
            | "exists"
            | "aexists"
            | "contains"
            | "acontains"
            | "aggregate"
            | "aaggregate"
            | "create"
            | "acreate"
            | "get_or_create"
            | "aget_or_create"
            | "update_or_create"
            | "aupdate_or_create"
            | "bulk_create"
            | "abulk_create"
            | "bulk_update"
            | "abulk_update"
            | "update"
            | "aupdate"
            | "delete"
            | "adelete"
            | "in_bulk"
            | "ain_bulk"
            | "values"
            | "values_list"
            | "dates"
            | "datetimes"
            | "explain"
            | "aexplain"
            | "raw"
    )
}

fn relation_write_method(method: &str) -> bool {
    matches!(
        method,
        "create"
            | "acreate"
            | "get_or_create"
            | "aget_or_create"
            | "update_or_create"
            | "aupdate_or_create"
            | "add"
            | "aadd"
            | "remove"
            | "aremove"
            | "clear"
            | "aclear"
            | "set"
            | "aset"
            | "update"
            | "aupdate"
            | "delete"
            | "adelete"
            | "bulk_create"
            | "abulk_create"
            | "bulk_update"
            | "abulk_update"
    )
}

fn literal_relation_paths_from_arguments(
    arguments: &[Expr],
    mut unknown: bool,
) -> (Vec<String>, bool) {
    let paths = arguments
        .iter()
        .filter_map(|argument| {
            let path = match argument {
                Expr::StringLiteral(literal) => Some(literal.value.to_str().to_string()),
                Expr::Call(prefetch) => {
                    let name = match prefetch.func.as_ref() {
                        Expr::Name(name) => name.id.as_str(),
                        Expr::Attribute(attribute) => attribute.attr.as_str(),
                        _ => return None,
                    };
                    let path = (name == "Prefetch")
                        .then(|| prefetch.arguments.args.first())
                        .flatten()
                        .and_then(|argument| match argument {
                            Expr::StringLiteral(literal) => {
                                Some(literal.value.to_str().to_string())
                            }
                            _ => None,
                        });
                    unknown |= !prefetch.arguments.keywords.is_empty()
                        || prefetch.arguments.args.len() != 1;
                    path
                }
                _ => None,
            };
            unknown |= path.is_none();
            path
        })
        .collect();
    (paths, unknown)
}

fn fetch_mode_from_expression(expression: &Expr) -> Option<FetchMode> {
    let name = match expression {
        Expr::Name(name) => name.id.as_str(),
        Expr::Attribute(attribute) => attribute.attr.as_str(),
        _ => return None,
    };
    match name {
        "FETCH_PEERS" => Some(FetchMode::Peers),
        "RAISE" => Some(FetchMode::Raise),
        "FETCH_ONE" => Some(FetchMode::One),
        _ => None,
    }
}

fn relation_is_loaded(query: &QueryState, path: &str, all_selectable: bool) -> bool {
    loading_state_covers(&query.loading, path, all_selectable)
}

fn loading_state_covers(loading: &LoadingState, path: &str, all_selectable: bool) -> bool {
    let covers = |loaded: &str| loaded == path || loaded.starts_with(&format!("{path}__"));
    loading.prefetched.iter().any(|loaded| covers(loaded))
        || loading.prefetched_unknown
        || (all_selectable
            && (loading.select_all
                || loading.selected_unknown
                || loading.selected.iter().any(|loaded| covers(loaded))
                || matches!(
                    loading.fetch_mode,
                    FetchMode::Peers | FetchMode::Raise | FetchMode::Unknown
                )))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::analysis::AnalysisDatabase;
    use crate::config::DjangoLspConfig;

    fn diagnostics(source: &str) -> Vec<OrmDiagnostic> {
        diagnostics_with_modules(source, &[])
    }

    fn diagnostics_with_modules(source: &str, modules: &[(&str, &str)]) -> Vec<OrmDiagnostic> {
        let directory = tempdir().unwrap();
        let app = directory.path().join("blog");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("models.py"),
            r#"
from django.db import models
from django.utils.functional import cached_property

class Team(models.Model):
    name = models.CharField()

class Author(models.Model):
    team = models.ForeignKey(Team, on_delete=models.CASCADE)

class Tag(models.Model):
    name = models.CharField()

class Blog(models.Model):
    title = models.CharField()
    author = models.ForeignKey(Author, on_delete=models.CASCADE, related_name="blogs")
    tags = models.ManyToManyField(Tag)

class Conference(models.Model):
    name = models.CharField()

class ReviewSession(models.Model):
    conference = models.ForeignKey(Conference, on_delete=models.CASCADE)

class Grant(models.Model):
    conference = models.ForeignKey(Conference, on_delete=models.CASCADE, related_name="grants")

    @property
    def total_grantee_reimbursement_amount(self):
        return sum(
            reimbursement.granted_amount
            for reimbursement in self.reimbursements.all()
            if reimbursement.category.name != "ticket"
        )

class ReimbursementCategory(models.Model):
    name = models.CharField()

class Reimbursement(models.Model):
    grant = models.ForeignKey(Grant, on_delete=models.CASCADE, related_name="reimbursements")
    category = models.ForeignKey(ReimbursementCategory, on_delete=models.CASCADE)
    granted_amount = models.IntegerField()

class User(models.Model):
    name = models.CharField()

class Keynote(models.Model):
    title = models.CharField()

    @property
    def speaker_names(self):
        keynote_speakers = [
            speaker for speaker in self.speakers.all() if speaker.user_id
        ]
        return [speaker.user.name for speaker in keynote_speakers]

class KeynoteSpeaker(models.Model):
    keynote = models.ForeignKey(Keynote, on_delete=models.CASCADE, related_name="speakers")
    user = models.ForeignKey(User, on_delete=models.CASCADE)

class Submission(models.Model):
    speaker = models.ForeignKey(User, on_delete=models.CASCADE)

class ScheduleItem(models.Model):
    submission = models.ForeignKey(Submission, on_delete=models.CASCADE, null=True)
    keynote = models.ForeignKey(Keynote, on_delete=models.CASCADE, null=True)

    @cached_property
    def speakers(self):
        speakers = []
        if self.submission_id:
            speakers.append(self.submission.speaker)
        if self.keynote_id:
            for speaker_keynote in self.keynote.speakers.order_by("id").all():
                speakers.append(speaker_keynote.user)
        speakers.extend(
            [speaker.user for speaker in self.additional_speakers.order_by("id").all()]
        )
        return speakers

class ScheduleItemAdditionalSpeaker(models.Model):
    schedule_item = models.ForeignKey(
        ScheduleItem, on_delete=models.CASCADE, related_name="additional_speakers"
    )
    user = models.ForeignKey(User, on_delete=models.CASCADE)
"#,
        )
        .unwrap();
        for (name, source) in modules {
            fs::write(app.join(name), source).unwrap();
        }
        let views = app.join("views.py");
        fs::write(&views, source).unwrap();
        let database =
            AnalysisDatabase::build(directory.path(), DjangoLspConfig::default()).unwrap();
        database.diagnostics_for_path(&views).unwrap().to_vec()
    }

    #[test]
    fn warns_for_relations_accessed_in_loops_and_comprehensions() {
        let result = diagnostics(
            r#"
from .models import Author, Blog

blogs = Blog.objects.filter(title__isnull=False)
for blog in blogs:
    print(blog.author.team.name)

emails = [author.blogs.count() for author in Author.objects.all()]
"#,
        );

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].relation_path, "author__team");
        assert_eq!(result[0].method, "select_related");
        assert_eq!(result[1].relation_path, "blogs");
        assert_eq!(result[1].method, "prefetch_related");
    }

    #[test]
    fn eager_loading_and_fetch_peers_suppress_warnings() {
        let result = diagnostics(
            r#"
from django.db import models
from .models import Author, Blog

for blog in Blog.objects.select_related("author__team"):
    print(blog.author.team.name)

for author in Author.objects.prefetch_related("blogs"):
    print(author.blogs.count())

for blog in Blog.objects.fetch_mode(models.FETCH_PEERS):
    print(blog.author.name)
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn all_comprehension_kinds_use_the_same_repeated_access_analysis() {
        let result = diagnostics(
            r#"
from .models import Blog

list_result = [blog.author for blog in Blog.objects.all()]
set_result = {blog.author for blog in Blog.objects.all()}
dict_result = {blog.title: blog.author for blog in Blog.objects.all()}
generator_result = (blog.author for blog in Blog.objects.all())
"#,
        );

        assert_eq!(result.len(), 4);
        assert!(result.iter().all(|item| item.relation_path == "author"));
    }

    #[test]
    fn understands_prefetch_objects_and_avoids_guessing_dynamic_configuration() {
        let result = diagnostics(
            r#"
from django.db.models import Prefetch
from .models import Author, Blog

for author in Author.objects.prefetch_related(Prefetch("blogs")):
    print(author.blogs.count())

for blog in Blog.objects.select_related(relation_name()):
    print(blog.author.name)

for author in Author.objects.prefetch_related(relation_name()):
    print(author.blogs.count())
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn understands_prefetch_related_objects_applied_before_iteration() {
        let result = diagnostics(
            r#"
from django.db.models import Prefetch, prefetch_related_objects
from .models import Author

authors = Author.objects.all()
prefetch_related_objects(authors, Prefetch("blogs"))

for author in authors:
    print(author.blogs.count())
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn warns_once_for_repeated_access_to_the_same_relation_in_one_iteration() {
        let result = diagnostics(
            r#"
from .models import Blog

for blog in Blog.objects.all():
    if blog.author:
        print(blog.author.email)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author");
    }

    #[test]
    fn propagates_nested_prefetch_state_into_related_manager_loops() {
        let result = diagnostics(
            r#"
from .models import Author

for author in Author.objects.prefetch_related("blogs__author"):
    for blog in author.blogs.all():
        print(blog.author.name)
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn preserves_loading_applied_to_the_inner_related_manager_queryset() {
        let result = diagnostics(
            r#"
from .models import Author

for author in Author.objects.all():
    for blog in author.blogs.select_related("author"):
        print(blog.author.name)

for author in Author.objects.prefetch_related("blogs"):
    for blog in author.blogs.select_related("author"):
        print(blog.author.name)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "blogs");
        assert_eq!(result[0].method, "prefetch_related");
    }

    #[test]
    fn reverse_foreign_key_managers_cache_the_parent_relation() {
        let result = diagnostics(
            r#"
from .models import Conference

def render_grants(conference: Conference):
    return [grant.conference.name for grant in conference.grants.all()]

for conference in Conference.objects.prefetch_related("grants"):
    for grant in conference.grants.all():
        print(grant.conference.name)
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn resolves_function_local_and_dotted_model_imports() {
        let result = diagnostics(
            r#"
def local_import():
    from .models import Blog as Post
    for blog in Post.objects.all():
        print(blog.author.name)

def dotted_import():
    import blog.models as models
    for blog in models.Blog.objects.all():
        print(blog.author.name)
"#,
        );

        assert_eq!(result.len(), 2, "{result:#?}");
    }

    #[test]
    fn follows_typed_model_parameters_related_managers_and_collection_wrappers() {
        let result = diagnostics(
            r#"
from .models import ReviewSession

def notify_grants(review_session: ReviewSession):
    grants = list(review_session.conference.grants.filter(active=True))
    for grant in grants:
        if grant.reimbursements.exists():
            for reimbursement in grant.reimbursements.all():
                print(reimbursement.category.name)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "reimbursements__category");
        assert_eq!(result[0].method, "prefetch_related");
    }

    #[test]
    fn follows_related_managers_from_django_model_self() {
        let result = diagnostics(
            r#"
from django.db import models
from .models import Team

class Parent(models.Model):
    def child_names(self):
        return [child.team.name for child in self.children.all()]

class Child(models.Model):
    parent = models.ForeignKey(Parent, on_delete=models.CASCADE, related_name="children")
    team = models.ForeignKey(Team, on_delete=models.CASCADE)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "team");
    }

    #[test]
    fn follows_custom_queryset_methods_but_not_terminal_projection_methods() {
        let result = diagnostics(
            r#"
from .models import Blog

for blog in Blog.objects.published().for_homepage():
    print(blog.author.name)

for row in Blog.objects.values("author"):
    print(row.author.name)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author");
    }

    #[test]
    fn follows_cross_module_and_transitive_helper_calls() {
        let result = diagnostics_with_modules(
            r#"
from .models import Blog
from .presenters import Card

for blog in Blog.objects.all():
    print(Card.from_model(blog))
"#,
            &[(
                "presenters.py",
                r#"
def author_name(blog):
    return blog.author.name

def render_blog(blog):
    return author_name(blog)

class Card:
    @classmethod
    def from_model(cls, blog):
        return render_blog(blog)
"#,
            )],
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author");
        assert_eq!(result[0].method, "select_related");
    }

    #[test]
    fn follows_django_model_instance_method_calls() {
        let result = diagnostics(
            r#"
from django.db import models
from .models import Author

class Article(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)

    def byline(self):
        return self.author.team.name

    def label(self):
        return self.byline()

for article in Article.objects.all():
    print(article.label())
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author__team");
    }

    #[test]
    fn helper_call_paths_compose_with_related_arguments() {
        let result = diagnostics(
            r#"
from .models import Blog

def team_name(author):
    return author.team.name

for blog in Blog.objects.all():
    print(team_name(blog.author))
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author__team");
    }

    #[test]
    fn pycon_properties_report_nested_paths_at_repeated_call_sites() {
        let source = r#"
from .models import Grant, Keynote

for grant in Grant.objects.all():
    print(grant.total_grantee_reimbursement_amount)

for keynote in Keynote.objects.all():
    print(keynote.speaker_names)
"#;
        let result = diagnostics(source);

        assert_eq!(result.len(), 2, "{result:#?}");
        assert_eq!(result[0].relation_path, "reimbursements__category");
        assert_eq!(result[0].method, "prefetch_related");
        assert_eq!(
            source_for_range(source, result[0].range),
            "total_grantee_reimbursement_amount"
        );
        assert_eq!(result[1].relation_path, "speakers__user");
        assert_eq!(result[1].method, "prefetch_related");
        assert_eq!(source_for_range(source, result[1].range), "speaker_names");
    }

    #[test]
    fn pycon_property_paths_respect_existing_nested_prefetches() {
        let result = diagnostics(
            r#"
from .models import Grant, Keynote

for grant in Grant.objects.prefetch_related("reimbursements__category"):
    print(grant.total_grantee_reimbursement_amount)

for keynote in Keynote.objects.prefetch_related("speakers__user"):
    print(keynote.speaker_names)
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn pycon_admin_property_reports_each_missing_eager_load_at_the_display_method() {
        let source = r#"
from django.contrib import admin
from .models import ScheduleItem

@admin.register(ScheduleItem)
class ScheduleItemAdmin(admin.ModelAdmin):
    def get_queryset(self, request):
        return super().get_queryset(request).prefetch_related("rooms")

    def speakers_names(self, obj: ScheduleItem):
        return ", ".join(speaker.name for speaker in obj.speakers)
"#;
        let result = diagnostics(source);

        assert_eq!(
            result
                .iter()
                .map(|diagnostic| diagnostic.relation_path.as_str())
                .collect::<Vec<_>>(),
            [
                "additional_speakers__user",
                "keynote__speakers__user",
                "submission__speaker",
            ],
            "{result:#?}"
        );
        assert!(
            result
                .iter()
                .all(|diagnostic| { source_for_range(source, diagnostic.range) == "speakers" })
        );
    }

    #[test]
    fn nested_related_manager_helpers_report_the_full_outer_path() {
        let source = r#"
from .models import Grant

def reimbursement_categories(grant):
    return [item.category.name for item in grant.reimbursements.all()]

for grant in Grant.objects.all():
    print(reimbursement_categories(grant))
"#;
        let result = diagnostics(source);

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "reimbursements__category");
        assert_eq!(result[0].method, "prefetch_related");
        assert_eq!(source_for_range(source, result[0].range), "grant");
    }

    #[test]
    fn nested_related_manager_loops_keep_the_outer_queryset_provenance() {
        let source = r#"
from .models import Keynote

for keynote in Keynote.objects.all():
    for speaker in keynote.speakers.all():
        print(speaker.user.name)
"#;
        let result = diagnostics(source);

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "speakers__user");
        assert_eq!(result[0].method, "prefetch_related");
        assert_eq!(
            source_for_range(source, result[0].range),
            "Keynote.objects.all()"
        );
    }

    fn source_for_range(source: &str, range: TextRange) -> &str {
        &source[range.start().to_usize()..range.end().to_usize()]
    }

    #[test]
    fn helper_summaries_respect_internal_prefetches_and_relation_writes() {
        let result = diagnostics(
            r#"
from django.db.models import prefetch_related_objects
from .models import Blog

FIELDS = ("author",)

def render_blog(blog):
    prefetch_related_objects([blog], *FIELDS)
    return blog.author.name

def attach_tag(blog, tag):
    blog.tags.add(tag)
    blog.tags.all().delete()

for blog in Blog.objects.all():
    print(render_blog(blog))
    attach_tag(blog, object())
    blog.tags.clear()
    blog.tags.all().delete()
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn django_admin_display_methods_inherit_get_queryset_eager_loading() {
        let result = diagnostics(
            r#"
from django.contrib import admin
from .models import Grant

@admin.register(Grant)
class GrantAdmin(admin.ModelAdmin):
    @admin.display
    def reimbursements(self, obj: Grant):
        return [item.category.name for item in obj.reimbursements.all()]

    def get_queryset(self, request):
        return super().get_queryset(request).prefetch_related("reimbursements__category")
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }

    #[test]
    fn django_admin_display_methods_warn_when_get_queryset_does_not_load_relation() {
        let result = diagnostics(
            r#"
from django.contrib import admin
from .models import Blog

@admin.register(Blog)
class BlogAdmin(admin.ModelAdmin):
    @admin.display
    def author_name(self, obj: Blog):
        return obj.author.name

    def get_queryset(self, request):
        return super().get_queryset(request)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author");
    }

    #[test]
    fn follows_queryset_return_annotations_without_guessing_collection_inputs() {
        let result = diagnostics(
            r#"
from django.db.models import QuerySet
from .models import Blog

def render_blogs(blogs: list[Blog]):
    return [blog.author.name for blog in blogs]

def published_blogs() -> QuerySet[Blog]:
    return Blog.objects.filter(published=True)

for blog in published_blogs()[:10]:
    print(blog.author.name)

def ready_blogs() -> QuerySet[Blog]:
    return Blog.objects.select_related("author")

for blog in ready_blogs():
    print(blog.author.name)
"#,
        );

        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(result[0].relation_path, "author");
    }

    #[test]
    fn ignores_scalars_non_repeated_access_and_unknown_values() {
        let result = diagnostics(
            r#"
from .models import Blog

blog = Blog.objects.get(pk=1)
print(blog.author.name)

for blog in make_blogs():
    print(blog.author.name)

for blog in Blog.objects.all():
    print(blog.title)
"#,
        );

        assert!(result.is_empty(), "{result:#?}");
    }
}
