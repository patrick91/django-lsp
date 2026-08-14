use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use ruff_python_ast as ast;
use ruff_python_ast::token::TokenKind;
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Expr, PySourceType, Stmt};
use ruff_python_parser::parse_unchecked_source;
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::config::DjangoLspConfig;

const GENERIC_LOOKUPS: &[&str] = &[
    "exact",
    "iexact",
    "icontains",
    "contains",
    "startswith",
    "istartswith",
    "endswith",
    "iendswith",
    "in",
    "isnull",
    "gt",
    "gte",
    "lt",
    "lte",
];

const RELATION_FIELD_NAMES: &[&str] = &["ForeignKey", "OneToOneField", "ManyToManyField"];
const DJANGO_MODEL_BASES: &[&str] = &[
    "django.db.models.Model",
    "django.contrib.auth.models.AbstractUser",
    "django.contrib.auth.base_user.AbstractBaseUser",
    "model_utils.models.StatusModel",
    "model_utils.models.TimeFramedModel",
    "model_utils.models.TimeStampedModel",
    "model_utils.models.UUIDModel",
    "ordered_model.models.OrderedModel",
    "wagtail.contrib.settings.models.BaseGenericSetting",
    "wagtail.contrib.settings.models.BaseSiteSetting",
    "wagtail.models.Page",
];
const QUERYSET_PRESERVING_METHODS: &[&str] = &[
    "all",
    "filter",
    "exclude",
    "order_by",
    "select_related",
    "prefetch_related",
    "fetch_mode",
    "distinct",
    "only",
    "defer",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelId(pub String);

impl ModelId {
    pub fn new(qualified_name: impl Into<String>) -> Self {
        Self(qualified_name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Scalar,
    ForeignKey,
    OneToOne,
    ManyToMany,
}

impl FieldKind {
    fn from_constructor_name(name: &str) -> Option<Self> {
        match name {
            "ForeignKey" => Some(Self::ForeignKey),
            "OneToOneField" => Some(Self::OneToOne),
            "ManyToManyField" => Some(Self::ManyToMany),
            other if other.ends_with("Field") => Some(Self::Scalar),
            _ => None,
        }
    }

    pub fn is_relation(self) -> bool {
        matches!(self, Self::ForeignKey | Self::OneToOne | Self::ManyToMany)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub kind: FieldKind,
    pub related_model: Option<ModelId>,
    pub relation_direction: Option<RelationDirection>,
    pub relation_accessor: Option<String>,
    pub supported_lookups: &'static [&'static str],
}

impl FieldInfo {
    pub fn supports_select_related(&self) -> bool {
        self.related_model.is_some()
            && matches!(
                (self.relation_direction, self.kind),
                (Some(RelationDirection::Forward), FieldKind::ForeignKey)
                    | (Some(RelationDirection::Forward), FieldKind::OneToOne)
                    | (Some(RelationDirection::Reverse), FieldKind::OneToOne)
            )
    }

    pub fn supports_prefetch_related(&self) -> bool {
        self.related_model.is_some() && self.relation_accessor.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: ModelId,
    pub module: String,
    pub class_name: String,
    pub fields: Vec<FieldInfo>,
}

impl ModelInfo {
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| field.name == name)
    }

    pub fn relation_for_accessor(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| {
            field.related_model.is_some()
                && field.relation_accessor.as_deref().unwrap_or(&field.name) == name
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInfo {
    pub path: PathBuf,
    pub module_name: String,
    pub is_package: bool,
    pub imports: HashMap<String, String>,
    pub model_names: HashMap<String, ModelId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleAnalysis {
    pub path: PathBuf,
    pub module_name: String,
    pub is_package: bool,
    pub imports: HashMap<String, String>,
    pub local_class_names: HashSet<String>,
    pub body: Vec<Stmt>,
    diagnostic_suppressions: Vec<DiagnosticSuppression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosticSuppression {
    line: TextRange,
    codes: Vec<String>,
}

impl ModuleAnalysis {
    pub(crate) fn suppresses_diagnostic(&self, code: &str, offset: TextSize) -> bool {
        self.diagnostic_suppressions.iter().any(|suppression| {
            suppression.line.contains(offset)
                && suppression.codes.iter().any(|ignored| ignored == code)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleFacts {
    path: PathBuf,
    module_name: String,
    is_package: bool,
    imports: HashMap<String, String>,
    local_class_names: HashSet<String>,
    raw_classes: Vec<RawClassInfo>,
    settings: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallablePath {
    pub parameter_index: usize,
    pub segments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableSummary {
    pub qualified_name: String,
    pub parameters: Vec<String>,
    pub bound_parameter_count: usize,
    pub paths: Vec<CallablePath>,
    pub return_collection_model: Option<String>,
    pub return_selected: Vec<String>,
    pub return_prefetched: Vec<String>,
    pub return_select_all: bool,
    pub return_selected_unknown: bool,
    pub return_prefetched_unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallableFacts {
    summaries: Vec<RawCallableSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallableIndex {
    summaries: HashMap<String, CallableSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawCallableSummary {
    qualified_name: String,
    parameters: Vec<String>,
    bound_parameter_count: usize,
    return_collection_model: Option<String>,
    return_loading: QueryLoadingState,
    paths: Vec<CallablePath>,
    calls: Vec<SummaryCall>,
    prefetches: HashMap<usize, SummaryPrefetchState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SummaryCall {
    target: String,
    receiver: Option<ParameterOrigin>,
    positional: Vec<Option<ParameterOrigin>>,
    keywords: Vec<(String, ParameterOrigin)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParameterOrigin {
    parameter_index: usize,
    prefix: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SummaryPrefetchState {
    paths: HashSet<String>,
    unknown: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct QueryLoadingState {
    selected: HashSet<String>,
    prefetched: HashSet<String>,
    select_all: bool,
    selected_unknown: bool,
    prefetched_unknown: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceIndex {
    root: PathBuf,
    pub modules: HashMap<PathBuf, ModuleInfo>,
    pub models: HashMap<ModelId, ModelInfo>,
    models_by_class_name: HashMap<String, Vec<ModelId>>,
    settings: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawClassInfo {
    id: ModelId,
    module_name: String,
    class_name: String,
    bases: Vec<String>,
    fields: Vec<PendingField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingField {
    name: String,
    kind: FieldKind,
    relation_target: Option<PendingRelationTarget>,
    reverse_query_name: Option<String>,
    reverse_accessor_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRelationTarget {
    Qualified(String),
    StringLiteral(String),
    SettingsKey(String),
}

impl WorkspaceIndex {
    pub(crate) fn from_facts(
        root: PathBuf,
        config: DjangoLspConfig,
        facts: &[&ModuleFacts],
    ) -> Self {
        let mut modules = HashMap::new();
        let mut raw_classes = Vec::new();
        let mut settings = HashMap::new();
        let mut facts = facts.to_vec();
        facts.sort_unstable_by(|left, right| left.path.cmp(&right.path));

        for facts in &facts {
            if is_settings_module(&facts.module_name, config.settings_module.as_deref()) {
                settings.extend(facts.settings.clone());
            }
            raw_classes.extend(facts.raw_classes.iter().cloned());
        }

        let mut model_ids = HashSet::new();
        loop {
            let mut changed = false;
            for class in &raw_classes {
                if model_ids.contains(&class.id) {
                    continue;
                }

                let is_model = class.bases.iter().any(|base| {
                    DJANGO_MODEL_BASES.contains(&base.as_str())
                        || model_ids.contains(&ModelId::new(base.clone()))
                });
                if is_model {
                    changed = true;
                    model_ids.insert(class.id.clone());
                }
            }

            if !changed {
                break;
            }
        }

        let mut models_by_class_name = HashMap::<String, Vec<ModelId>>::new();
        for model_id in &model_ids {
            if let Some(class_name) = model_id.0.rsplit('.').next() {
                models_by_class_name
                    .entry(class_name.to_string())
                    .or_default()
                    .push(model_id.clone());
            }
        }

        let mut models = HashMap::new();
        for class in raw_classes
            .iter()
            .filter(|class| model_ids.contains(&class.id))
        {
            let class_id = class.id.clone();
            let class_module = class.module_name.clone();
            let class_name = class.class_name.clone();
            let fields = class
                .fields
                .iter()
                .map(|field| FieldInfo {
                    name: field.name.clone(),
                    kind: field.kind,
                    related_model: field.relation_target.as_ref().and_then(|target| {
                        resolve_relation_target(
                            target,
                            &class_id,
                            &class_module,
                            &model_ids,
                            &models_by_class_name,
                            &settings,
                        )
                    }),
                    relation_direction: field
                        .kind
                        .is_relation()
                        .then_some(RelationDirection::Forward),
                    relation_accessor: field.kind.is_relation().then(|| field.name.clone()),
                    supported_lookups: GENERIC_LOOKUPS,
                })
                .collect();

            models.insert(
                class_id.clone(),
                ModelInfo {
                    id: class_id,
                    module: class_module,
                    class_name,
                    fields,
                },
            );
        }

        for class in raw_classes
            .iter()
            .filter(|class| model_ids.contains(&class.id))
        {
            for field in &class.fields {
                let Some(reverse_query_name) = field.reverse_query_name.as_ref() else {
                    continue;
                };
                let Some(reverse_accessor_name) = field.reverse_accessor_name.as_ref() else {
                    continue;
                };

                let Some(target) = field.relation_target.as_ref().and_then(|target| {
                    resolve_relation_target(
                        target,
                        &class.id,
                        &class.module_name,
                        &model_ids,
                        &models_by_class_name,
                        &settings,
                    )
                }) else {
                    continue;
                };

                let Some(target_model) = models.get_mut(&target) else {
                    continue;
                };

                if target_model
                    .fields
                    .iter()
                    .any(|existing| existing.name == *reverse_query_name)
                {
                    continue;
                }

                target_model.fields.push(FieldInfo {
                    name: reverse_query_name.clone(),
                    kind: field.kind,
                    related_model: Some(class.id.clone()),
                    relation_direction: Some(RelationDirection::Reverse),
                    relation_accessor: Some(reverse_accessor_name.clone()),
                    supported_lookups: GENERIC_LOOKUPS,
                });
            }
        }

        for facts in &facts {
            let mut model_names = HashMap::new();
            for class_name in &facts.local_class_names {
                let model_id = ModelId::new(format!("{}.{}", facts.module_name, class_name));
                if models.contains_key(&model_id) {
                    model_names.insert(class_name.clone(), model_id);
                }
            }

            modules.insert(
                facts.path.clone(),
                ModuleInfo {
                    path: facts.path.clone(),
                    module_name: facts.module_name.clone(),
                    is_package: facts.is_package,
                    imports: facts.imports.clone(),
                    model_names,
                },
            );
        }

        Self {
            root,
            modules,
            models,
            models_by_class_name,
            settings,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn setting(&self, name: &str) -> Option<&str> {
        self.settings.get(name).map(String::as_str)
    }

    pub fn module_for_path(&self, path: &Path) -> Option<&ModuleInfo> {
        self.modules.get(path)
    }

    pub fn model(&self, model_id: &ModelId) -> Option<&ModelInfo> {
        self.models.get(model_id)
    }

    pub fn resolve_model_symbol(&self, module_name: &str, local_name: &str) -> Option<ModelId> {
        let module = self
            .modules
            .values()
            .find(|module| module.module_name == module_name)?;
        self.resolve_model_symbol_in_module(module, local_name)
    }

    pub fn resolve_model_symbol_in_module(
        &self,
        module: &ModuleInfo,
        local_name: &str,
    ) -> Option<ModelId> {
        module.model_names.get(local_name).cloned().or_else(|| {
            module.imports.get(local_name).and_then(|qualified| {
                self.resolve_qualified_model_inner(qualified, &mut HashSet::new())
            })
        })
    }

    pub fn resolve_qualified_model(&self, qualified: &str) -> Option<ModelId> {
        self.resolve_qualified_model_inner(qualified, &mut HashSet::new())
    }

    fn resolve_qualified_model_inner(
        &self,
        qualified: &str,
        visited: &mut HashSet<String>,
    ) -> Option<ModelId> {
        let candidate = ModelId::new(qualified.to_string());
        if self.models.contains_key(&candidate) {
            return Some(candidate);
        }

        if !visited.insert(qualified.to_string()) {
            return None;
        }

        if let Some((module_name, local_name)) = qualified.rsplit_once('.')
            && let Some(module) = self
                .modules
                .values()
                .find(|module| module.module_name == module_name)
        {
            if let Some(model_id) = module.model_names.get(local_name) {
                return Some(model_id.clone());
            }

            if let Some(reexported) = module.imports.get(local_name)
                && let Some(model_id) = self.resolve_qualified_model_inner(reexported, visited)
            {
                return Some(model_id);
            }
        }

        if let Some((app_label, class_name)) = qualified.split_once('.') {
            let candidates = self.models_by_class_name.get(class_name)?;
            let matching = candidates
                .iter()
                .filter(|candidate| candidate.0.split('.').next() == Some(app_label))
                .cloned()
                .collect::<Vec<_>>();

            if matching.len() == 1 {
                return matching.into_iter().next();
            }

            return matching
                .into_iter()
                .find(|candidate| candidate.0.ends_with(&format!(".models.{class_name}")));
        }

        let candidates = self.models_by_class_name.get(qualified)?;
        if candidates.len() == 1 {
            candidates.first().cloned()
        } else {
            None
        }
    }
}

impl CallableIndex {
    pub(crate) fn from_facts(facts: &[&CallableFacts]) -> Self {
        const MAX_SUMMARY_DEPTH: usize = 8;

        let raw = facts
            .iter()
            .flat_map(|facts| facts.summaries.iter().cloned())
            .map(|summary| (summary.qualified_name.clone(), summary))
            .collect::<HashMap<_, _>>();
        let mut paths = raw
            .iter()
            .map(|(name, summary)| {
                (
                    name.clone(),
                    summary.paths.iter().cloned().collect::<HashSet<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();

        for _ in 0..MAX_SUMMARY_DEPTH {
            let snapshot = paths.clone();
            let mut changed = false;
            for (caller_name, caller) in &raw {
                let Some(caller_paths) = paths.get_mut(caller_name) else {
                    continue;
                };
                for call in &caller.calls {
                    let Some(callee) = raw.get(&call.target) else {
                        continue;
                    };
                    let Some(callee_paths) = snapshot.get(&call.target) else {
                        continue;
                    };
                    for callee_path in callee_paths {
                        let Some(origin) =
                            call.parameter_origin(callee, callee_path.parameter_index)
                        else {
                            continue;
                        };
                        let mut segments = origin.prefix.clone();
                        segments.extend(callee_path.segments.iter().cloned());
                        if segments.is_empty() || segments.len() > MAX_SUMMARY_DEPTH {
                            continue;
                        }
                        if caller.prefetch_covers(origin.parameter_index, &segments) {
                            continue;
                        }
                        changed |= caller_paths.insert(CallablePath {
                            parameter_index: origin.parameter_index,
                            segments,
                        });
                    }
                }
            }
            if !changed {
                break;
            }
        }

        let summaries = raw
            .into_iter()
            .map(|(name, raw)| {
                let mut summary_paths = paths
                    .remove(&name)
                    .unwrap_or_default()
                    .into_iter()
                    .collect::<Vec<_>>();
                summary_paths.sort();
                (
                    name.clone(),
                    CallableSummary {
                        qualified_name: name,
                        parameters: raw.parameters,
                        bound_parameter_count: raw.bound_parameter_count,
                        paths: summary_paths,
                        return_collection_model: raw.return_collection_model,
                        return_selected: sorted_strings(raw.return_loading.selected),
                        return_prefetched: sorted_strings(raw.return_loading.prefetched),
                        return_select_all: raw.return_loading.select_all,
                        return_selected_unknown: raw.return_loading.selected_unknown,
                        return_prefetched_unknown: raw.return_loading.prefetched_unknown,
                    },
                )
            })
            .collect();
        Self { summaries }
    }

    pub fn summary(&self, qualified_name: &str) -> Option<&CallableSummary> {
        self.summaries.get(qualified_name)
    }
}

impl RawCallableSummary {
    fn prefetch_covers(&self, parameter_index: usize, segments: &[String]) -> bool {
        let Some(prefetch) = self.prefetches.get(&parameter_index) else {
            return false;
        };
        if prefetch.unknown {
            return true;
        }
        let path = segments.join("__");
        prefetch
            .paths
            .iter()
            .any(|loaded| path == *loaded || path.starts_with(&format!("{loaded}__")))
    }
}

impl SummaryCall {
    fn parameter_origin(
        &self,
        callee: &RawCallableSummary,
        parameter_index: usize,
    ) -> Option<&ParameterOrigin> {
        if parameter_index < callee.bound_parameter_count {
            return (parameter_index == 0)
                .then_some(self.receiver.as_ref())
                .flatten();
        }
        let positional_index = parameter_index.checked_sub(callee.bound_parameter_count)?;
        self.positional
            .get(positional_index)
            .and_then(Option::as_ref)
            .or_else(|| {
                let parameter_name = callee.parameters.get(parameter_index)?;
                self.keywords
                    .iter()
                    .find_map(|(name, origin)| (name == parameter_name).then_some(origin))
            })
    }
}

pub fn analyze_source(root: &Path, path: &Path, source: &str) -> ModuleAnalysis {
    let parsed = parse_unchecked_source(source, PySourceType::from(path));
    let diagnostic_suppressions = parsed
        .tokens()
        .iter()
        .filter(|token| token.kind() == TokenKind::Comment)
        .filter_map(|token| {
            let comment =
                source.get(token.range().start().to_usize()..token.range().end().to_usize())?;
            let codes = suppression_codes(comment)?;
            Some(DiagnosticSuppression {
                line: source_line_range(source, token.start()),
                codes,
            })
        })
        .collect();
    let syntax = parsed.syntax().clone();
    let is_package = path.file_name().and_then(|name| name.to_str()) == Some("__init__.py");
    let module_name = module_name_from_path(root, path);
    let imports = collect_imports(&module_name, is_package, &syntax.body);
    let local_class_names = syntax
        .body
        .iter()
        .filter_map(|statement| match statement {
            Stmt::ClassDef(class_def) => Some(class_def.name.id.to_string()),
            _ => None,
        })
        .collect();

    ModuleAnalysis {
        path: path.to_path_buf(),
        module_name,
        is_package,
        imports,
        local_class_names,
        body: syntax.body.to_vec(),
        diagnostic_suppressions,
    }
}

fn suppression_codes(comment: &str) -> Option<Vec<String>> {
    let directive = comment.strip_prefix('#')?.trim();
    let codes = directive.strip_prefix("django-lsp: ignore[")?;
    let (codes, _) = codes.split_once(']')?;
    let codes = codes
        .split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!codes.is_empty()).then_some(codes)
}

fn source_line_range(source: &str, offset: TextSize) -> TextRange {
    let offset = offset.to_usize();
    let start = source[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let end = source[offset..]
        .find('\n')
        .map_or(source.len(), |newline| offset + newline);
    TextRange::new(
        TextSize::try_from(start).expect("Python source offsets fit in TextSize"),
        TextSize::try_from(end).expect("Python source offsets fit in TextSize"),
    )
}

pub(crate) fn facts_from_analysis(analysis: &ModuleAnalysis) -> ModuleFacts {
    ModuleFacts {
        path: analysis.path.clone(),
        module_name: analysis.module_name.clone(),
        is_package: analysis.is_package,
        imports: analysis.imports.clone(),
        local_class_names: analysis.local_class_names.clone(),
        raw_classes: extract_raw_classes(analysis),
        settings: extract_settings_assignments(&analysis.body),
    }
}

pub(crate) fn callable_facts_from_analysis(analysis: &ModuleAnalysis) -> CallableFacts {
    let local_functions = analysis
        .body
        .iter()
        .filter_map(|statement| match statement {
            Stmt::FunctionDef(function) => Some(function.name.id.to_string()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut summaries = Vec::new();
    for statement in &analysis.body {
        match statement {
            Stmt::FunctionDef(function) => summaries.push(extract_callable_summary(
                analysis,
                &local_functions,
                None,
                function,
            )),
            Stmt::ClassDef(class) => {
                for statement in &class.body {
                    if let Stmt::FunctionDef(function) = statement {
                        summaries.push(extract_callable_summary(
                            analysis,
                            &local_functions,
                            Some(class.name.as_str()),
                            function,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    CallableFacts { summaries }
}

fn extract_callable_summary(
    analysis: &ModuleAnalysis,
    local_functions: &HashSet<String>,
    class_name: Option<&str>,
    function: &ast::StmtFunctionDef,
) -> RawCallableSummary {
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| parameter.name().to_string())
        .collect::<Vec<_>>();
    let parameter_indices = parameters
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index))
        .collect();
    let bound_parameter_count = usize::from(
        class_name.is_some()
            && !function.decorator_list.iter().any(|decorator| {
                qualify_expr(
                    &decorator.expression,
                    &analysis.module_name,
                    &analysis.local_class_names,
                    &analysis.imports,
                )
                .is_some_and(|name| name.rsplit('.').next() == Some("staticmethod"))
            }),
    );
    let mut visitor = CallableSummaryVisitor {
        module_name: &analysis.module_name,
        class_name,
        local_class_names: &analysis.local_class_names,
        imports: &analysis.imports,
        local_functions,
        parameter_indices,
        paths: HashSet::new(),
        calls: Vec::new(),
        prefetches: HashMap::new(),
    };
    for statement in &function.body {
        visitor.visit_stmt(statement);
    }
    let mut paths = visitor
        .paths
        .into_iter()
        .filter(|path| {
            let Some(prefetch) = visitor.prefetches.get(&path.parameter_index) else {
                return true;
            };
            if prefetch.unknown {
                return false;
            }
            let path = path.segments.join("__");
            !prefetch
                .paths
                .iter()
                .any(|loaded| path == *loaded || path.starts_with(&format!("{loaded}__")))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let qualified_name = class_name.map_or_else(
        || format!("{}.{}", analysis.module_name, function.name.id),
        |class_name| format!("{}.{class_name}.{}", analysis.module_name, function.name.id),
    );
    RawCallableSummary {
        qualified_name,
        parameters,
        bound_parameter_count,
        return_collection_model: function
            .returns
            .as_deref()
            .and_then(|annotation| collection_annotation_model_name(annotation, analysis)),
        return_loading: return_query_loading(function),
        paths,
        calls: visitor.calls,
        prefetches: visitor.prefetches,
    }
}

fn collection_annotation_model_name(
    annotation: &Expr,
    analysis: &ModuleAnalysis,
) -> Option<String> {
    match annotation {
        Expr::BinOp(binary) => collection_annotation_model_name(&binary.left, analysis)
            .or_else(|| collection_annotation_model_name(&binary.right, analysis)),
        Expr::Subscript(subscript) => {
            let wrapper = qualify_expr(
                &subscript.value,
                &analysis.module_name,
                &analysis.local_class_names,
                &analysis.imports,
            )?;
            let wrapper = wrapper.rsplit('.').next()?;
            if matches!(wrapper, "Optional" | "Union" | "Annotated") {
                return collection_annotation_model_name(&subscript.slice, analysis);
            }
            if !matches!(wrapper, "QuerySet" | "Manager" | "BaseManager") {
                return None;
            }
            let item = match subscript.slice.as_ref() {
                Expr::Tuple(tuple) => tuple.elts.first()?,
                item => item,
            };
            qualify_expr(
                item,
                &analysis.module_name,
                &analysis.local_class_names,
                &analysis.imports,
            )
        }
        Expr::Tuple(tuple) => tuple
            .elts
            .iter()
            .find_map(|item| collection_annotation_model_name(item, analysis)),
        _ => None,
    }
}

fn return_query_loading(function: &ast::StmtFunctionDef) -> QueryLoadingState {
    let mut collector = ReturnQueryLoadingCollector { states: Vec::new() };
    for statement in &function.body {
        collector.visit_stmt(statement);
    }
    let mut states = collector.states.into_iter();
    let Some(mut merged) = states.next() else {
        return QueryLoadingState::default();
    };
    for state in states {
        match (merged.select_all, state.select_all) {
            (true, false) => {
                merged.selected = state.selected;
                merged.select_all = false;
            }
            (false, false) => merged.selected.retain(|path| state.selected.contains(path)),
            _ => {}
        }
        merged
            .prefetched
            .retain(|path| state.prefetched.contains(path));
        merged.selected_unknown |= state.selected_unknown;
        merged.prefetched_unknown |= state.prefetched_unknown;
    }
    merged
}

struct ReturnQueryLoadingCollector {
    states: Vec<QueryLoadingState>,
}

impl Visitor<'_> for ReturnQueryLoadingCollector {
    fn visit_stmt(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Return(return_statement) => {
                let mut state = QueryLoadingState::default();
                if let Some(value) = &return_statement.value {
                    QueryLoadingVisitor { state: &mut state }.visit_expr(value);
                }
                self.states.push(state);
            }
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            _ => visitor::walk_stmt(self, statement),
        }
    }
}

struct QueryLoadingVisitor<'a> {
    state: &'a mut QueryLoadingState,
}

impl Visitor<'_> for QueryLoadingVisitor<'_> {
    fn visit_expr(&mut self, expression: &Expr) {
        if let Expr::Call(call) = expression
            && let Expr::Attribute(method) = call.func.as_ref()
        {
            match method.attr.as_str() {
                "select_related" => {
                    if call.arguments.args.is_empty() {
                        self.state.select_all = true;
                    } else if call
                        .arguments
                        .args
                        .first()
                        .is_some_and(|argument| matches!(argument, Expr::NoneLiteral(_)))
                    {
                        self.state.selected.clear();
                        self.state.select_all = false;
                        self.state.selected_unknown = false;
                    } else {
                        let (paths, unknown) = query_loading_paths(call);
                        self.state.selected.extend(paths);
                        self.state.selected_unknown |= unknown;
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
                        self.state.prefetched.clear();
                        self.state.prefetched_unknown = false;
                    } else {
                        let (paths, unknown) = query_loading_paths(call);
                        self.state.prefetched.extend(paths);
                        self.state.prefetched_unknown |= unknown;
                    }
                }
                _ => {}
            }
        }
        visitor::walk_expr(self, expression);
    }
}

fn query_loading_paths(call: &ast::ExprCall) -> (Vec<String>, bool) {
    let mut unknown = !call.arguments.keywords.is_empty();
    let paths = call
        .arguments
        .args
        .iter()
        .filter_map(|argument| {
            let path = match argument {
                Expr::StringLiteral(literal) => Some(literal.value.to_str().to_string()),
                Expr::Call(prefetch) => {
                    let is_prefetch = match prefetch.func.as_ref() {
                        Expr::Name(name) => name.id.as_str() == "Prefetch",
                        Expr::Attribute(attribute) => attribute.attr.as_str() == "Prefetch",
                        _ => false,
                    };
                    is_prefetch
                        .then(|| prefetch.arguments.args.first())
                        .flatten()
                        .and_then(|argument| match argument {
                            Expr::StringLiteral(literal) => {
                                Some(literal.value.to_str().to_string())
                            }
                            _ => None,
                        })
                }
                _ => None,
            };
            unknown |= path.is_none();
            path
        })
        .collect();
    (paths, unknown)
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

struct CallableSummaryVisitor<'a> {
    module_name: &'a str,
    class_name: Option<&'a str>,
    local_class_names: &'a HashSet<String>,
    imports: &'a HashMap<String, String>,
    local_functions: &'a HashSet<String>,
    parameter_indices: HashMap<String, usize>,
    paths: HashSet<CallablePath>,
    calls: Vec<SummaryCall>,
    prefetches: HashMap<usize, SummaryPrefetchState>,
}

impl Visitor<'_> for CallableSummaryVisitor<'_> {
    fn visit_stmt(&mut self, statement: &Stmt) {
        if matches!(statement, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            return;
        }
        visitor::walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &Expr) {
        if let Expr::Call(call) = expression
            && let Expr::Attribute(method) = call.func.as_ref()
            && relation_write_method(method.attr.as_str())
        {
            for argument in &call.arguments.args {
                self.visit_expr(argument);
            }
            for keyword in &call.arguments.keywords {
                self.visit_expr(&keyword.value);
            }
            return;
        }
        if let Some(origin) = parameter_origin(expression, &self.parameter_indices)
            && !origin.prefix.is_empty()
        {
            if origin
                .prefix
                .last()
                .is_some_and(|segment| relation_write_method(segment))
            {
                return;
            }
            self.paths.insert(CallablePath {
                parameter_index: origin.parameter_index,
                segments: origin.prefix,
            });
            return;
        }

        if let Expr::Call(call) = expression {
            self.record_prefetch_related_objects(call);
            let receiver = match call.func.as_ref() {
                Expr::Attribute(method) => parameter_origin(&method.value, &self.parameter_indices),
                _ => None,
            };
            let same_class_target = match (call.func.as_ref(), self.class_name) {
                (Expr::Attribute(method), Some(class_name)) if matches!(method.value.as_ref(), Expr::Name(name) if matches!(name.id.as_str(), "self" | "cls")) => {
                    Some(format!("{}.{class_name}.{}", self.module_name, method.attr))
                }
                _ => None,
            };
            let target = same_class_target.or_else(|| {
                qualify_expr(
                    &call.func,
                    self.module_name,
                    self.local_class_names,
                    self.imports,
                )
                .map(|target| {
                    if !target.contains('.') && self.local_functions.contains(&target) {
                        format!("{}.{}", self.module_name, target)
                    } else {
                        target
                    }
                })
            });
            if let Some(target) = target {
                let positional = call
                    .arguments
                    .args
                    .iter()
                    .map(|argument| parameter_origin(argument, &self.parameter_indices))
                    .collect();
                let keywords = call
                    .arguments
                    .keywords
                    .iter()
                    .filter_map(|keyword| {
                        Some((
                            keyword.arg.as_ref()?.to_string(),
                            parameter_origin(&keyword.value, &self.parameter_indices)?,
                        ))
                    })
                    .collect();
                self.calls.push(SummaryCall {
                    target,
                    receiver,
                    positional,
                    keywords,
                });
            }
        }

        visitor::walk_expr(self, expression);
    }
}

impl CallableSummaryVisitor<'_> {
    fn record_prefetch_related_objects(&mut self, call: &ast::ExprCall) {
        let Some(target) = qualify_expr(
            &call.func,
            self.module_name,
            self.local_class_names,
            self.imports,
        ) else {
            return;
        };
        if !matches!(
            target.as_str(),
            "django.db.models.prefetch_related_objects"
                | "django.db.models.query.prefetch_related_objects"
        ) {
            return;
        }
        let Some(objects) = call.arguments.args.first() else {
            return;
        };
        let object_expressions: &[Expr] = match objects {
            Expr::List(list) => &list.elts,
            Expr::Tuple(tuple) => &tuple.elts,
            Expr::Set(set) => &set.elts,
            expression => std::slice::from_ref(expression),
        };
        let parameter_indices = object_expressions
            .iter()
            .filter_map(|expression| {
                let origin = parameter_origin(expression, &self.parameter_indices)?;
                origin.prefix.is_empty().then_some(origin.parameter_index)
            })
            .collect::<Vec<_>>();
        if parameter_indices.is_empty() {
            return;
        }

        let mut paths = Vec::new();
        let mut unknown = !call.arguments.keywords.is_empty();
        for argument in call.arguments.args.iter().skip(1) {
            if let Expr::StringLiteral(literal) = argument {
                paths.push(literal.value.to_str().to_string());
            } else {
                unknown = true;
            }
        }
        for parameter_index in parameter_indices {
            let state = self.prefetches.entry(parameter_index).or_default();
            state.paths.extend(paths.iter().cloned());
            state.unknown |= unknown;
        }
    }
}

fn parameter_origin(
    expression: &Expr,
    parameter_indices: &HashMap<String, usize>,
) -> Option<ParameterOrigin> {
    let mut current = expression;
    let mut prefix = Vec::new();
    while let Expr::Attribute(attribute) = current {
        prefix.push(attribute.attr.to_string());
        current = &attribute.value;
    }
    let Expr::Name(root) = current else {
        return None;
    };
    prefix.reverse();
    Some(ParameterOrigin {
        parameter_index: *parameter_indices.get(root.id.as_str())?,
        prefix,
    })
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

fn module_name_from_path(root: &Path, path: &Path) -> String {
    let Ok(relative) = path.strip_prefix(root) else {
        return path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("__root__")
            .to_string();
    };

    let mut components = relative
        .iter()
        .map(|component| component.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if components.is_empty() {
        return "__root__".to_string();
    }

    if components
        .last()
        .is_some_and(|component| component == "__init__.py")
    {
        components.pop();
    } else if let Some(last) = components.last_mut()
        && let Some(stem) = Path::new(last).file_stem().and_then(|stem| stem.to_str())
    {
        *last = stem.to_string();
    }

    if components.is_empty() {
        "__root__".to_string()
    } else {
        components.join(".")
    }
}

fn collect_imports(module_name: &str, is_package: bool, body: &[Stmt]) -> HashMap<String, String> {
    let mut imports = HashMap::new();
    for statement in body {
        apply_import_statement(statement, module_name, is_package, &mut imports);
    }
    imports
}

pub(crate) fn apply_import_statement(
    statement: &Stmt,
    module_name: &str,
    is_package: bool,
    imports: &mut HashMap<String, String>,
) {
    match statement {
        Stmt::Import(import_stmt) => {
            for alias in &import_stmt.names {
                if let Some(asname) = &alias.asname {
                    imports.insert(asname.id.to_string(), alias.name.id.to_string());
                } else {
                    let local_name = alias
                        .name
                        .id
                        .split('.')
                        .next()
                        .unwrap_or(alias.name.as_str());
                    imports.insert(local_name.to_string(), local_name.to_string());
                }
            }
        }
        Stmt::ImportFrom(import_from) => {
            let Some(module) = resolve_import_module(
                module_name,
                is_package,
                import_from.level,
                import_from.module.as_ref().map(|module| module.as_str()),
            ) else {
                return;
            };

            for alias in &import_from.names {
                if alias.name.as_str() == "*" {
                    continue;
                }

                let local_name = alias
                    .asname
                    .as_ref()
                    .map(|asname| asname.id.to_string())
                    .unwrap_or_else(|| alias.name.id.to_string());
                imports.insert(local_name, format!("{module}.{}", alias.name.id));
            }
        }
        _ => {}
    }
}

fn is_settings_module(module_name: &str, configured_module: Option<&str>) -> bool {
    configured_module.map_or_else(
        || module_name.rsplit('.').next() == Some("settings"),
        |configured| module_name == configured,
    )
}

fn resolve_import_module(
    module_name: &str,
    is_package: bool,
    level: u32,
    imported_module: Option<&str>,
) -> Option<String> {
    if level == 0 {
        return imported_module.map(ToOwned::to_owned);
    }

    let package = if is_package {
        module_name.to_string()
    } else {
        module_name
            .rsplit_once('.')
            .map(|(package, _)| package.to_string())
            .unwrap_or_default()
    };

    let mut parts = if package.is_empty() {
        Vec::new()
    } else {
        package
            .split('.')
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    };

    for _ in 0..level.saturating_sub(1) {
        parts.pop()?;
    }

    if let Some(imported) = imported_module {
        parts.extend(imported.split('.').map(ToOwned::to_owned));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

fn extract_raw_classes(analysis: &ModuleAnalysis) -> Vec<RawClassInfo> {
    analysis
        .body
        .iter()
        .filter_map(|statement| match statement {
            Stmt::ClassDef(class_def) => Some(extract_raw_class(
                &analysis.module_name,
                &analysis.local_class_names,
                &analysis.imports,
                class_def,
            )),
            _ => None,
        })
        .collect()
}

fn extract_settings_assignments(body: &[Stmt]) -> HashMap<String, String> {
    let mut settings = HashMap::new();
    for statement in body {
        match statement {
            Stmt::Assign(assign) => {
                if assign.targets.len() != 1 {
                    continue;
                }

                let Expr::Name(name) = &assign.targets[0] else {
                    continue;
                };

                if !name
                    .id
                    .chars()
                    .all(|character| character.is_ascii_uppercase() || character == '_')
                {
                    continue;
                }

                if let Some(value) = expr_string_value(&assign.value) {
                    settings.insert(name.id.to_string(), value);
                }
            }
            Stmt::AnnAssign(assign) => {
                let Expr::Name(name) = assign.target.as_ref() else {
                    continue;
                };

                if !name
                    .id
                    .chars()
                    .all(|character| character.is_ascii_uppercase() || character == '_')
                {
                    continue;
                }

                if let Some(value) = assign
                    .value
                    .as_ref()
                    .and_then(|value| expr_string_value(value))
                {
                    settings.insert(name.id.to_string(), value);
                }
            }
            _ => {}
        }
    }
    settings
}

fn extract_raw_class(
    module_name: &str,
    local_class_names: &HashSet<String>,
    imports: &HashMap<String, String>,
    class_def: &ast::StmtClassDef,
) -> RawClassInfo {
    let bases = class_def
        .bases()
        .iter()
        .filter_map(|base| qualify_expr(base, module_name, local_class_names, imports))
        .collect();
    let fields = class_def
        .body
        .iter()
        .filter_map(|statement| {
            extract_field(
                statement,
                module_name,
                &class_def.name.id,
                local_class_names,
                imports,
            )
        })
        .collect();

    RawClassInfo {
        id: ModelId::new(format!("{module_name}.{}", class_def.name.id)),
        module_name: module_name.to_string(),
        class_name: class_def.name.id.to_string(),
        bases,
        fields,
    }
}

fn extract_field(
    statement: &Stmt,
    module_name: &str,
    class_name: &str,
    local_class_names: &HashSet<String>,
    imports: &HashMap<String, String>,
) -> Option<PendingField> {
    match statement {
        Stmt::Assign(assign) => {
            if assign.targets.len() != 1 {
                return None;
            }

            let target_name = match &assign.targets[0] {
                Expr::Name(name) => name.id.as_str(),
                _ => return None,
            };

            extract_field_from_value(
                target_name,
                &assign.value,
                module_name,
                class_name,
                local_class_names,
                imports,
            )
        }
        Stmt::AnnAssign(assign) => {
            let target_name = match assign.target.as_ref() {
                Expr::Name(name) => name.id.as_str(),
                _ => return None,
            };

            assign.value.as_ref().and_then(|value| {
                extract_field_from_value(
                    target_name,
                    value,
                    module_name,
                    class_name,
                    local_class_names,
                    imports,
                )
            })
        }
        _ => None,
    }
}

fn extract_field_from_value(
    target_name: &str,
    value: &Expr,
    module_name: &str,
    class_name: &str,
    local_class_names: &HashSet<String>,
    imports: &HashMap<String, String>,
) -> Option<PendingField> {
    let call = match value {
        Expr::Call(call) => call,
        _ => return None,
    };

    let qualified = qualify_expr(&call.func, module_name, local_class_names, imports)?;
    let constructor_name = qualified.rsplit('.').next()?;
    let kind = FieldKind::from_constructor_name(constructor_name)?;

    let relation_target = if RELATION_FIELD_NAMES.contains(&constructor_name) {
        call.arguments.args.first().and_then(|target| {
            extract_relation_target(target, module_name, class_name, local_class_names, imports)
        })
    } else {
        None
    };

    let (reverse_query_name, reverse_accessor_name) =
        reverse_relation_names(kind, class_name, &call.arguments.keywords);

    Some(PendingField {
        name: target_name.to_string(),
        kind,
        relation_target,
        reverse_query_name,
        reverse_accessor_name,
    })
}

fn extract_relation_target(
    expr: &Expr,
    module_name: &str,
    class_name: &str,
    local_class_names: &HashSet<String>,
    imports: &HashMap<String, String>,
) -> Option<PendingRelationTarget> {
    match expr {
        Expr::StringLiteral(string_literal) => Some(PendingRelationTarget::StringLiteral(
            string_literal.value.to_str().to_string(),
        )),
        Expr::Attribute(attribute) if matches!(attribute.value.as_ref(), Expr::Name(name) if name.id.as_str() == "settings") => {
            Some(PendingRelationTarget::SettingsKey(
                attribute.attr.id.to_string(),
            ))
        }
        Expr::Name(_) | Expr::Attribute(_) => {
            qualify_expr(expr, module_name, local_class_names, imports)
                .map(PendingRelationTarget::Qualified)
        }
        _ => {
            let _ = class_name;
            None
        }
    }
}

fn reverse_relation_names(
    kind: FieldKind,
    class_name: &str,
    keywords: &[ast::Keyword],
) -> (Option<String>, Option<String>) {
    if !kind.is_relation() {
        return (None, None);
    }

    let related_name = keyword_string_value(keywords, "related_name");
    if related_name
        .as_deref()
        .is_some_and(|name| name.contains('+'))
    {
        return (None, None);
    }

    let default_query_name = class_name.to_lowercase();
    let query_name = keyword_string_value(keywords, "related_query_name")
        .or_else(|| related_name.clone())
        .unwrap_or_else(|| default_query_name.clone());
    let accessor_name = related_name.unwrap_or_else(|| match kind {
        FieldKind::OneToOne => default_query_name,
        FieldKind::ForeignKey | FieldKind::ManyToMany => format!("{default_query_name}_set"),
        FieldKind::Scalar => unreachable!("scalar fields do not have reverse relations"),
    });

    (Some(query_name), Some(accessor_name))
}

fn keyword_string_value(keywords: &[ast::Keyword], expected_name: &str) -> Option<String> {
    keywords.iter().find_map(|keyword| {
        let arg = keyword.arg.as_ref()?;
        if arg.as_str() != expected_name {
            return None;
        }

        match &keyword.value {
            Expr::StringLiteral(string_literal) => Some(string_literal.value.to_str().to_string()),
            _ => None,
        }
    })
}

fn expr_string_value(expr: &Expr) -> Option<String> {
    match expr {
        Expr::StringLiteral(string_literal) => Some(string_literal.value.to_str().to_string()),
        _ => None,
    }
}

pub fn qualify_expr(
    expr: &Expr,
    module_name: &str,
    local_class_names: &HashSet<String>,
    imports: &HashMap<String, String>,
) -> Option<String> {
    match expr {
        Expr::Name(name) => {
            if let Some(qualified) = imports.get(name.id.as_str()) {
                Some(qualified.clone())
            } else if local_class_names.contains(name.id.as_str()) {
                Some(format!("{module_name}.{}", name.id))
            } else {
                Some(name.id.to_string())
            }
        }
        Expr::Attribute(attribute) => {
            let base = qualify_expr(&attribute.value, module_name, local_class_names, imports)?;
            Some(format!("{base}.{}", attribute.attr))
        }
        _ => None,
    }
}

fn resolve_relation_target(
    target: &PendingRelationTarget,
    class_id: &ModelId,
    class_module: &str,
    model_ids: &HashSet<ModelId>,
    models_by_class_name: &HashMap<String, Vec<ModelId>>,
    settings: &HashMap<String, String>,
) -> Option<ModelId> {
    match target {
        PendingRelationTarget::Qualified(qualified) => {
            let candidate = ModelId::new(qualified.clone());
            if model_ids.contains(&candidate) {
                return Some(candidate);
            }

            resolve_app_label_model(qualified, models_by_class_name)
        }
        PendingRelationTarget::StringLiteral(value) => {
            if value == "self" {
                return Some(class_id.clone());
            }

            if value.contains('.') {
                let direct = ModelId::new(value.clone());
                if model_ids.contains(&direct) {
                    return Some(direct);
                }

                return resolve_app_label_model(value, models_by_class_name);
            }

            let same_module = ModelId::new(format!("{class_module}.{value}"));
            if model_ids.contains(&same_module) {
                return Some(same_module);
            }

            let candidates = models_by_class_name.get(value)?;
            if candidates.len() == 1 {
                candidates.first().cloned()
            } else {
                None
            }
        }
        PendingRelationTarget::SettingsKey(setting_name) => {
            let value = settings.get(setting_name)?;
            let direct = ModelId::new(value.clone());
            if model_ids.contains(&direct) {
                return Some(direct);
            }

            resolve_app_label_model(value, models_by_class_name)
        }
    }
}

fn resolve_app_label_model(
    qualified: &str,
    models_by_class_name: &HashMap<String, Vec<ModelId>>,
) -> Option<ModelId> {
    let (app_label, class_name) = qualified.split_once('.')?;
    let candidates = models_by_class_name.get(class_name)?;
    let matching = candidates
        .iter()
        .filter(|candidate| candidate.0.split('.').next() == Some(app_label))
        .cloned()
        .collect::<Vec<_>>();

    if matching.len() == 1 {
        return matching.into_iter().next();
    }

    matching
        .into_iter()
        .find(|candidate| candidate.0.ends_with(&format!(".models.{class_name}")))
}

pub fn infer_model_for_expression(
    expr: &Expr,
    index: &WorkspaceIndex,
    module_name: &str,
    imports: &HashMap<String, String>,
    local_bindings: &HashMap<String, ModelId>,
) -> Option<ModelId> {
    match expr {
        Expr::Name(_) => {
            resolve_model_reference_expr(expr, index, module_name, imports, local_bindings)
        }
        Expr::Attribute(attribute) if attribute.attr.as_str() == "objects" => {
            infer_model_for_expression(
                &attribute.value,
                index,
                module_name,
                imports,
                local_bindings,
            )
        }
        Expr::Attribute(_) => {
            resolve_model_reference_expr(expr, index, module_name, imports, local_bindings)
        }
        Expr::Call(call) => {
            let method = match call.func.as_ref() {
                Expr::Attribute(attribute) => attribute,
                _ => return None,
            };

            if QUERYSET_PRESERVING_METHODS.contains(&method.attr.as_str()) {
                infer_model_for_expression(
                    &method.value,
                    index,
                    module_name,
                    imports,
                    local_bindings,
                )
            } else {
                None
            }
        }
        _ => None,
    }
}

fn resolve_model_reference_expr(
    expr: &Expr,
    index: &WorkspaceIndex,
    module_name: &str,
    imports: &HashMap<String, String>,
    local_bindings: &HashMap<String, ModelId>,
) -> Option<ModelId> {
    match expr {
        Expr::Name(name) => local_bindings
            .get(name.id.as_str())
            .cloned()
            .or_else(|| index.resolve_model_symbol(module_name, name.id.as_str()))
            .or_else(|| {
                imports
                    .get(name.id.as_str())
                    .and_then(|qualified| index.resolve_qualified_model(qualified))
            }),
        Expr::Attribute(_) => {
            let qualified = qualify_expr(expr, module_name, &HashSet::new(), imports)?;
            index.resolve_qualified_model(&qualified)
        }
        _ => None,
    }
}

pub fn collect_visible_scope(
    body: &[Stmt],
    cursor_offset: usize,
    index: &WorkspaceIndex,
    module_name: &str,
    is_package: bool,
    imports: &mut HashMap<String, String>,
    bindings: &mut HashMap<String, ModelId>,
) {
    let cursor = TextSize::try_from(cursor_offset).unwrap_or(TextSize::from(u32::MAX));
    for statement in body {
        if statement.range().start() > cursor {
            break;
        }
        collect_scope_from_statement(
            statement,
            cursor,
            index,
            module_name,
            is_package,
            imports,
            bindings,
        );
    }
}

fn collect_scope_from_statement(
    statement: &Stmt,
    cursor: TextSize,
    index: &WorkspaceIndex,
    module_name: &str,
    is_package: bool,
    imports: &mut HashMap<String, String>,
    bindings: &mut HashMap<String, ModelId>,
) {
    match statement {
        Stmt::Import(_) | Stmt::ImportFrom(_) if statement.range().end() <= cursor => {
            apply_import_statement(statement, module_name, is_package, imports);
        }
        Stmt::Assign(assign) if assign.range.end() <= cursor && assign.targets.len() == 1 => {
            if let Expr::Name(name) = &assign.targets[0]
                && let Some(model_id) =
                    infer_model_for_expression(&assign.value, index, module_name, imports, bindings)
            {
                bindings.insert(name.id.to_string(), model_id);
            }
        }
        Stmt::AnnAssign(assign) if assign.range.end() <= cursor => {
            if let (Expr::Name(name), Some(value)) = (assign.target.as_ref(), assign.value.as_ref())
                && let Some(model_id) =
                    infer_model_for_expression(value, index, module_name, imports, bindings)
            {
                bindings.insert(name.id.to_string(), model_id);
            }
        }
        Stmt::If(if_stmt) => {
            collect_visible_scope(
                &if_stmt.body,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
            for clause in &if_stmt.elif_else_clauses {
                collect_visible_scope(
                    &clause.body,
                    cursor.to_usize(),
                    index,
                    module_name,
                    is_package,
                    imports,
                    bindings,
                );
            }
        }
        Stmt::For(for_stmt) => {
            collect_visible_scope(
                &for_stmt.body,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
            collect_visible_scope(
                &for_stmt.orelse,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
        }
        Stmt::While(while_stmt) => {
            collect_visible_scope(
                &while_stmt.body,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
            collect_visible_scope(
                &while_stmt.orelse,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
        }
        Stmt::With(with_stmt) => {
            collect_visible_scope(
                &with_stmt.body,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
        }
        Stmt::Try(try_stmt) => {
            collect_visible_scope(
                &try_stmt.body,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
            collect_visible_scope(
                &try_stmt.orelse,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
            collect_visible_scope(
                &try_stmt.finalbody,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
            for handler in &try_stmt.handlers {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                collect_visible_scope(
                    &handler.body,
                    cursor.to_usize(),
                    index,
                    module_name,
                    is_package,
                    imports,
                    bindings,
                );
            }
        }
        Stmt::FunctionDef(function_def) if function_def.range.contains_inclusive(cursor) => {
            collect_visible_scope(
                &function_def.body,
                cursor.to_usize(),
                index,
                module_name,
                is_package,
                imports,
                bindings,
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::ops::Deref;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::analysis::AnalysisDatabase;

    struct TestIndex(AnalysisDatabase);

    impl Deref for TestIndex {
        type Target = WorkspaceIndex;

        fn deref(&self) -> &Self::Target {
            self.0.index()
        }
    }

    fn build_index(root: &Path) -> TestIndex {
        TestIndex(AnalysisDatabase::build(root, DjangoLspConfig::default()).unwrap())
    }

    #[test]
    fn extracts_models_and_relations() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("blog");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("__init__.py"), "").unwrap();
        fs::write(
            app_dir.join("models.py"),
            r#"
from django.db import models

class Team(models.Model):
    name = models.CharField(max_length=64)

class Author(models.Model):
    email = models.EmailField()
    team = models.ForeignKey("Team", on_delete=models.CASCADE)

class Blog(models.Model):
    title = models.CharField(max_length=255)
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
"#,
        )
        .unwrap();

        let index = build_index(dir.path());
        let blog = index.model(&ModelId::new("blog.models.Blog")).unwrap();
        let author = blog.field("author").unwrap();
        assert_eq!(author.kind, FieldKind::ForeignKey);
        assert_eq!(
            author.related_model.as_ref().unwrap().as_str(),
            "blog.models.Author"
        );

        let author_model = index.model(author.related_model.as_ref().unwrap()).unwrap();
        let team = author_model.field("team").unwrap();
        assert_eq!(
            team.related_model.as_ref().unwrap().as_str(),
            "blog.models.Team"
        );
    }

    #[test]
    fn resolves_imported_models() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("blog");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("__init__.py"), "").unwrap();
        fs::write(
            app_dir.join("models.py"),
            r#"
from django.db import models

class Blog(models.Model):
    title = models.CharField(max_length=255)
"#,
        )
        .unwrap();
        fs::write(
            app_dir.join("views.py"),
            r#"
from .models import Blog as Post
"#,
        )
        .unwrap();

        let index = build_index(dir.path());
        let module = index.module_for_path(&app_dir.join("views.py")).unwrap();
        assert_eq!(
            index
                .resolve_model_symbol_in_module(module, "Post")
                .unwrap()
                .as_str(),
            "blog.models.Blog"
        );
    }

    #[test]
    fn updates_changed_file_analysis_and_restores_disk_contents() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("blog");
        let models_path = app_dir.join("models.py");
        let views_path = app_dir.join("views.py");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("__init__.py"), "").unwrap();
        fs::write(
            &models_path,
            "from django.db import models\nclass Blog(models.Model):\n    title = models.CharField()\n",
        )
        .unwrap();
        fs::write(&views_path, "from .models import Blog\n").unwrap();

        let mut index = build_index(dir.path());
        assert!(
            index
                .0
                .sync_path(
                    models_path.clone(),
                    Some(
                        "from django.db import models\nclass Blog(models.Model):\n    title = models.CharField()\n    summary = models.TextField()\n"
                            .to_string(),
                    ),
                )
                .unwrap()
        );
        assert!(
            index
                .model(&ModelId::new("blog.models.Blog"))
                .unwrap()
                .field("summary")
                .is_some()
        );

        assert!(index.0.sync_path_from_disk(models_path).unwrap());
        assert!(
            index
                .model(&ModelId::new("blog.models.Blog"))
                .unwrap()
                .field("summary")
                .is_none()
        );
    }

    #[test]
    fn detects_models_with_unaliased_dotted_imports() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("blog");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(
            app_dir.join("models.py"),
            r#"
import django.db.models

class Blog(django.db.models.Model):
    title = django.db.models.CharField(max_length=255)
"#,
        )
        .unwrap();

        let index = build_index(dir.path());
        let blog = index.model(&ModelId::new("blog.models.Blog")).unwrap();
        assert!(blog.field("title").is_some());
    }

    #[test]
    fn detects_models_from_common_third_party_abstract_bases() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("blog");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(
            app_dir.join("models.py"),
            r#"
from django.db import models
from model_utils.models import TimeStampedModel
from ordered_model.models import OrderedModel
from wagtail.models import Page

class Post(TimeStampedModel, OrderedModel):
    author = models.ForeignKey("Author", on_delete=models.CASCADE)

class Author(TimeStampedModel):
    name = models.CharField(max_length=64)

class ContentPage(Page):
    featured_post = models.ForeignKey(Post, on_delete=models.CASCADE)
"#,
        )
        .unwrap();

        let index = build_index(dir.path());
        for model in ["Post", "Author", "ContentPage"] {
            assert!(
                index
                    .model(&ModelId::new(format!("blog.models.{model}")))
                    .is_some(),
                "expected {model} to be indexed"
            );
        }
        assert_eq!(
            index
                .model(&ModelId::new("blog.models.Post"))
                .unwrap()
                .field("author")
                .unwrap()
                .related_model
                .as_ref()
                .unwrap()
                .as_str(),
            "blog.models.Author"
        );
    }

    #[test]
    fn extracts_auth_user_model_setting_and_user_model() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("core");
        let config_dir = dir.path().join("config");
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(app_dir.join("__init__.py"), "").unwrap();
        fs::write(config_dir.join("__init__.py"), "").unwrap();
        fs::write(
            config_dir.join("settings.py"),
            "AUTH_USER_MODEL = 'core.User'\n",
        )
        .unwrap();
        fs::write(
            app_dir.join("models.py"),
            r#"
from django.contrib.auth.models import AbstractUser
from django.db import models
from django.conf import settings

class User(AbstractUser):
    email = models.EmailField(unique=True)

class Route(models.Model):
    installer = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE)
"#,
        )
        .unwrap();

        let index = build_index(dir.path());
        assert_eq!(index.setting("AUTH_USER_MODEL"), Some("core.User"));
        assert!(index.model(&ModelId::new("core.models.User")).is_some());
        let route = index.model(&ModelId::new("core.models.Route")).unwrap();
        assert_eq!(
            route
                .field("installer")
                .unwrap()
                .related_model
                .as_ref()
                .unwrap()
                .as_str(),
            "core.models.User"
        );
    }

    #[test]
    fn ignores_settings_names_from_unrelated_modules() {
        let dir = tempdir().unwrap();
        for package in ["config", "core", "zzz"] {
            let package_dir = dir.path().join(package);
            fs::create_dir_all(&package_dir).unwrap();
            fs::write(package_dir.join("__init__.py"), "").unwrap();
        }
        fs::write(
            dir.path().join("config/settings.py"),
            "AUTH_USER_MODEL = 'core.User'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("zzz/constants.py"),
            "AUTH_USER_MODEL = 'zzz.OtherUser'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("core/models.py"),
            r#"
from django.conf import settings
from django.contrib.auth.models import AbstractUser
from django.db import models

class User(AbstractUser):
    email = models.EmailField()

class Route(models.Model):
    installer = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE)
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("zzz/models.py"),
            r#"
from django.contrib.auth.models import AbstractUser
from django.db import models

class OtherUser(AbstractUser):
    nickname = models.CharField(max_length=32)
"#,
        )
        .unwrap();

        let index = build_index(dir.path());
        let route = index.model(&ModelId::new("core.models.Route")).unwrap();
        assert_eq!(
            route
                .field("installer")
                .unwrap()
                .related_model
                .as_ref()
                .unwrap()
                .as_str(),
            "core.models.User"
        );
    }

    #[test]
    fn supports_an_explicit_settings_module() {
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("settings.py"),
            "AUTH_USER_MODEL = 'core.DefaultUser'\n",
        )
        .unwrap();
        fs::write(
            project_dir.join("production.py"),
            "AUTH_USER_MODEL = 'core.ProductionUser'\n",
        )
        .unwrap();

        let config = DjangoLspConfig {
            settings_module: Some("project.production".to_string()),
            ..DjangoLspConfig::default()
        };
        let index = TestIndex(AnalysisDatabase::build(dir.path(), config).unwrap());

        assert_eq!(
            index.setting("AUTH_USER_MODEL"),
            Some("core.ProductionUser")
        );
    }

    #[test]
    fn excludes_nested_environment_directories() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("app");
        let environment_dir = dir.path().join("nested/venv/ghost");
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(&environment_dir).unwrap();
        fs::write(
            app_dir.join("models.py"),
            "from django.db import models\nclass Real(models.Model):\n    name = models.CharField()\n",
        )
        .unwrap();
        fs::write(
            environment_dir.join("models.py"),
            "from django.db import models\nclass Ghost(models.Model):\n    name = models.CharField()\n",
        )
        .unwrap();

        let index = build_index(dir.path());
        assert!(index.model(&ModelId::new("app.models.Real")).is_some());
        assert!(
            index
                .models
                .values()
                .all(|model| model.class_name != "Ghost")
        );
    }
}
