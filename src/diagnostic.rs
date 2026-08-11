use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};
use ruff_text_size::{Ranged, TextRange};

use crate::index::{ModelId, ModuleAnalysis, WorkspaceIndex, apply_import_statement, qualify_expr};

pub const MISSING_EAGER_LOAD: &str = "DJ001";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrmDiagnostic {
    pub code: &'static str,
    pub range: TextRange,
    pub message: String,
    pub method: &'static str,
    pub relation_path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FetchMode {
    #[default]
    One,
    Peers,
    Raise,
    Unknown,
}

#[derive(Debug, Clone)]
struct QueryState {
    model: ModelId,
    selected: HashSet<String>,
    prefetched: HashSet<String>,
    iteration_range: Option<TextRange>,
    select_all: bool,
    selected_unknown: bool,
    prefetched_unknown: bool,
    fetch_mode: FetchMode,
}

impl QueryState {
    fn new(model: ModelId) -> Self {
        Self {
            model,
            selected: HashSet::new(),
            prefetched: HashSet::new(),
            iteration_range: None,
            select_all: false,
            selected_unknown: false,
            prefetched_unknown: false,
            fetch_mode: FetchMode::One,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Scope {
    imports: HashMap<String, String>,
    queries: HashMap<String, QueryState>,
    repeated_items: HashMap<String, QueryState>,
}

pub fn analyze_diagnostics(
    index: &WorkspaceIndex,
    analysis: &ModuleAnalysis,
) -> Vec<OrmDiagnostic> {
    let mut analyzer = Analyzer {
        index,
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
    analyzer.diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.relation_path.clone(),
        )
    });
    analyzer.diagnostics
}

struct Analyzer<'a> {
    index: &'a WorkspaceIndex,
    analysis: &'a ModuleAnalysis,
    diagnostics: Vec<OrmDiagnostic>,
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
                self.analyze_body(&function.body, &mut function_scope);
            }
            Stmt::ClassDef(class) => {
                let mut class_scope = Scope {
                    imports: scope.imports.clone(),
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
        let Some(query) = scope.repeated_items.get(root) else {
            return false;
        };
        let Some(mut model) = self.index.model(&query.model) else {
            return true;
        };

        let mut relation_path = Vec::new();
        let mut all_selectable = true;
        let mut diagnostic_range = None;
        for (segment, range) in segments {
            let Some(field) = model.relation_for_accessor(segment) else {
                break;
            };
            relation_path.push(segment);
            all_selectable &= field.supports_select_related();
            diagnostic_range = Some(range);
            let Some(related_model) = field
                .related_model
                .as_ref()
                .and_then(|model_id| self.index.model(model_id))
            else {
                break;
            };
            model = related_model;
        }

        let Some(range) = diagnostic_range else {
            return true;
        };
        let relation_path = relation_path.join("__");
        if relation_is_loaded(query, &relation_path, all_selectable) {
            return true;
        }

        let method = if all_selectable {
            "select_related"
        } else {
            "prefetch_related"
        };
        let iteration_range = query.iteration_range.unwrap_or(range);
        if !self.seen.insert((iteration_range, relation_path.clone())) {
            return true;
        }
        self.diagnostics.push(OrmDiagnostic {
            code: MISSING_EAGER_LOAD,
            range,
            message: format!(
                "Accessing `{relation_path}` for each `{}` may issue an extra query per row; add `{method}(\"{relation_path}\")` to the QuerySet.",
                self.index
                    .model(&query.model)
                    .map_or(query.model.as_str(), |model| model.class_name.as_str())
            ),
            method,
            relation_path,
        });
        true
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
    }

    fn remove_binding(&self, target: &Expr, scope: &mut Scope) {
        if let Expr::Name(name) = target {
            scope.queries.remove(name.id.as_str());
            scope.repeated_items.remove(name.id.as_str());
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
        if let Some(mut query) = query {
            query.iteration_range = Some(iteration_range);
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
        query.prefetched.extend(paths);
        query.prefetched_unknown |= unknown;
    }

    fn resolve_query_state(&self, expression: &Expr, scope: &Scope) -> Option<QueryState> {
        match expression {
            Expr::Name(name) => scope.queries.get(name.id.as_str()).cloned(),
            Expr::Attribute(attribute) if attribute.attr.as_str() == "objects" => {
                let model = self.resolve_model_reference(&attribute.value, scope)?;
                Some(QueryState::new(model))
            }
            Expr::Attribute(_) => self.resolve_related_manager(expression, scope),
            Expr::Call(call) => {
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
                        if call.arguments.args.is_empty() {
                            query.select_all = true;
                        } else if call
                            .arguments
                            .args
                            .first()
                            .is_some_and(|argument| matches!(argument, Expr::NoneLiteral(_)))
                        {
                            query.selected.clear();
                            query.select_all = false;
                            query.selected_unknown = false;
                        } else {
                            let (paths, unknown) = literal_relation_paths(call);
                            query.selected.extend(paths);
                            query.selected_unknown |= unknown;
                        }
                    }
                    "prefetch_related" => {
                        if call.arguments.args.len() == 1
                            && call
                                .arguments
                                .args
                                .first()
                                .is_some_and(|argument| matches!(argument, Expr::NoneLiteral(_)))
                        {
                            query.prefetched.clear();
                            query.prefetched_unknown = false;
                        } else {
                            let (paths, unknown) = literal_relation_paths(call);
                            query.prefetched.extend(paths);
                            query.prefetched_unknown |= unknown;
                        }
                    }
                    "fetch_mode" => {
                        query.fetch_mode = call
                            .arguments
                            .args
                            .first()
                            .and_then(fetch_mode_from_expression)
                            .unwrap_or(FetchMode::Unknown);
                    }
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
        let repeated = scope.repeated_items.get(root)?;
        let mut model = self.index.model(&repeated.model)?;
        let mut relation = None;
        let mut manager_path = Vec::new();
        for (segment, _) in segments {
            let field = model.relation_for_accessor(segment)?;
            let related_model = field.related_model.as_ref()?;
            relation = Some(field);
            manager_path.push(segment);
            model = self.index.model(related_model)?;
        }
        let relation = relation?;
        (!relation.supports_select_related()).then(|| {
            let mut query = QueryState::new(model.id.clone());
            let prefix = format!("{}__", manager_path.join("__"));
            query.prefetched.extend(
                repeated
                    .prefetched
                    .iter()
                    .filter_map(|path| path.strip_prefix(&prefix).map(ToOwned::to_owned)),
            );
            query.prefetched_unknown = repeated.prefetched_unknown;
            query
        })
    }
}

struct ExpressionAnalyzer<'a, 'b, 'scope> {
    analyzer: &'a mut Analyzer<'b>,
    scope: &'scope Scope,
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

fn literal_relation_paths(call: &ast::ExprCall) -> (Vec<String>, bool) {
    literal_relation_paths_from_arguments(&call.arguments.args, !call.arguments.keywords.is_empty())
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
    let covers = |loaded: &str| loaded == path || loaded.starts_with(&format!("{path}__"));
    query.prefetched.iter().any(|loaded| covers(loaded))
        || query.prefetched_unknown
        || (all_selectable
            && (query.select_all
                || query.selected_unknown
                || query.selected.iter().any(|loaded| covers(loaded))
                || matches!(
                    query.fetch_mode,
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
        let directory = tempdir().unwrap();
        let app = directory.path().join("blog");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("models.py"),
            r#"
from django.db import models

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
"#,
        )
        .unwrap();
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
