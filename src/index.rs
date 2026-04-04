use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use ruff_python_ast as ast;
use ruff_python_ast::{Expr, PySourceType, Stmt};
use ruff_python_parser::parse_unchecked_source;
use ruff_text_size::{Ranged, TextSize};

use crate::config::DjangoLspConfig;
use crate::document_store::DocumentStore;
use crate::error::{DjangoLspError, Result};

const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    "node_modules/**",
    "dist/**",
    "build/**",
    ".venv/**",
    "venv/**",
    "site-packages/**",
    "__pycache__/**",
    "**/migrations/**",
];

const GENERIC_LOOKUPS: &[&str] = &[
    "exact",
    "iexact",
    "contains",
    "icontains",
    "in",
    "gt",
    "gte",
    "lt",
    "lte",
    "isnull",
    "startswith",
    "istartswith",
    "endswith",
    "iendswith",
];

const RELATION_FIELD_NAMES: &[&str] = &["ForeignKey", "OneToOneField", "ManyToManyField"];
const DJANGO_MODEL_BASES: &[&str] = &[
    "django.db.models.Model",
    "django.contrib.auth.models.AbstractUser",
    "django.contrib.auth.base_user.AbstractBaseUser",
];
const QUERYSET_PRESERVING_METHODS: &[&str] = &[
    "all",
    "filter",
    "exclude",
    "order_by",
    "select_related",
    "prefetch_related",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub kind: FieldKind,
    pub related_model: Option<ModelId>,
    pub supported_lookups: &'static [&'static str],
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceIndex {
    root: PathBuf,
    config: DjangoLspConfig,
    pub modules: HashMap<PathBuf, ModuleInfo>,
    pub models: HashMap<ModelId, ModelInfo>,
    models_by_class_name: HashMap<String, Vec<ModelId>>,
    settings: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct RawClassInfo {
    id: ModelId,
    module_name: String,
    class_name: String,
    bases: Vec<String>,
    fields: Vec<PendingField>,
}

#[derive(Debug, Clone)]
struct PendingField {
    name: String,
    kind: FieldKind,
    relation_target: Option<PendingRelationTarget>,
    reverse_query_name: Option<String>,
}

#[derive(Debug, Clone)]
enum PendingRelationTarget {
    Qualified(String),
    StringLiteral(String),
    SettingsKey(String),
}

#[derive(Debug)]
struct PathMatcher {
    root: PathBuf,
    include: Option<GlobSet>,
    exclude: GlobSet,
}

impl PathMatcher {
    fn new(root: &Path, config: &DjangoLspConfig) -> Result<Self> {
        let include = if config.include.is_empty() {
            None
        } else {
            Some(build_globset(root, &config.include)?)
        };

        let mut exclude_patterns = DEFAULT_EXCLUDES.iter().map(|pattern| pattern.to_string()).collect::<Vec<_>>();
        exclude_patterns.extend(config.exclude.iter().cloned());
        let exclude = build_globset(root, &exclude_patterns)?;

        Ok(Self {
            root: root.to_path_buf(),
            include,
            exclude,
        })
    }

    fn matches(&self, path: &Path) -> bool {
        if path.extension().and_then(|ext| ext.to_str()) != Some("py") {
            return false;
        }

        let relative_path = path.strip_prefix(&self.root).unwrap_or(path);
        if self.exclude.is_match(relative_path) {
            return false;
        }

        self.include
            .as_ref()
            .map(|include| include.is_match(relative_path))
            .unwrap_or(true)
    }
}

fn build_globset(root: &Path, patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|source| DjangoLspError::glob(pattern.clone(), source))?);
        if !pattern.starts_with("**/") && !pattern.starts_with('.') && !pattern.contains('/') {
            let scoped = format!("**/{pattern}");
            builder.add(Glob::new(&scoped).map_err(|source| DjangoLspError::glob(scoped.clone(), source))?);
        }
    }
    let _ = root;
    builder.build().map_err(|source| DjangoLspError::glob("globset".to_string(), source))
}

impl WorkspaceIndex {
    pub fn build(workspace_root: &Path, config: DjangoLspConfig, documents: &DocumentStore) -> Result<Self> {
        let root = config.effective_root(workspace_root);
        let matcher = PathMatcher::new(&root, &config)?;
        let mut builder = WalkBuilder::new(&root);
        builder.hidden(false);
        builder.git_ignore(true);
        builder.git_global(true);
        builder.git_exclude(true);
        builder.parents(true);

        let mut analyses = Vec::new();
        let mut modules = HashMap::new();
        let mut raw_classes = Vec::new();
        let mut settings = HashMap::new();

        for entry in builder.build() {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if !entry.file_type().is_some_and(|file_type| file_type.is_file()) || !matcher.matches(path) {
                continue;
            }

            let source = if let Some(snapshot) = documents.get(path) {
                snapshot.text.clone()
            } else {
                match fs::read_to_string(path) {
                    Ok(contents) => contents,
                    Err(_) => continue,
                }
            };

            let analysis = analyze_source(&root, path, &source);
            settings.extend(extract_settings_assignments(&analysis.body));
            raw_classes.extend(extract_raw_classes(&analysis));
            analyses.push(analysis);
        }

        let mut model_ids = HashSet::new();
        loop {
            let mut changed = false;
            for class in &raw_classes {
                if model_ids.contains(&class.id) {
                    continue;
                }

                let is_model = class
                    .bases
                    .iter()
                    .any(|base| DJANGO_MODEL_BASES.contains(&base.as_str()) || model_ids.contains(&ModelId::new(base.clone())));
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
        for class in raw_classes.iter().filter(|class| model_ids.contains(&class.id)) {
            let class_id = class.id.clone();
            let class_module = class.module_name.clone();
            let class_name = class.class_name.clone();
            let fields = class
                .fields
                .iter()
                .map(|field| FieldInfo {
                    name: field.name.clone(),
                    kind: field.kind,
                    related_model: field
                        .relation_target
                        .as_ref()
                        .and_then(|target| {
                            resolve_relation_target(
                                target,
                                &class_id,
                                &class_module,
                                &model_ids,
                                &models_by_class_name,
                                &settings,
                            )
                        }),
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

        for class in raw_classes.iter().filter(|class| model_ids.contains(&class.id)) {
            for field in &class.fields {
                let Some(reverse_query_name) = field.reverse_query_name.as_ref() else {
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
                    supported_lookups: GENERIC_LOOKUPS,
                });
            }
        }

        for analysis in analyses {
            let mut model_names = HashMap::new();
            for class_name in &analysis.local_class_names {
                let model_id = ModelId::new(format!("{}.{}", analysis.module_name, class_name));
                if models.contains_key(&model_id) {
                    model_names.insert(class_name.clone(), model_id);
                }
            }

            modules.insert(
                analysis.path.clone(),
                ModuleInfo {
                    path: analysis.path,
                    module_name: analysis.module_name,
                    is_package: analysis.is_package,
                    imports: analysis.imports,
                    model_names,
                },
            );
        }

        Ok(Self {
            root,
            config,
            modules,
            models,
            models_by_class_name,
            settings,
        })
    }

    pub fn empty(root: PathBuf, config: DjangoLspConfig) -> Self {
        Self {
            root,
            config,
            modules: HashMap::new(),
            models: HashMap::new(),
            models_by_class_name: HashMap::new(),
            settings: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &DjangoLspConfig {
        &self.config
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
        let module = self.modules.values().find(|module| module.module_name == module_name)?;
        self.resolve_model_symbol_in_module(module, local_name)
    }

    pub fn resolve_model_symbol_in_module(&self, module: &ModuleInfo, local_name: &str) -> Option<ModelId> {
        module
            .model_names
            .get(local_name)
            .cloned()
            .or_else(|| module.imports.get(local_name).and_then(|qualified| self.resolve_qualified_model(qualified)))
    }

    pub fn resolve_qualified_model(&self, qualified: &str) -> Option<ModelId> {
        let candidate = ModelId::new(qualified.to_string());
        if self.models.contains_key(&candidate) {
            return Some(candidate);
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

    pub fn analyze_source(&self, path: &Path, source: &str) -> ModuleAnalysis {
        analyze_source(&self.root, path, source)
    }
}

pub fn analyze_source(root: &Path, path: &Path, source: &str) -> ModuleAnalysis {
    let parsed = parse_unchecked_source(source, PySourceType::from(path));
    let syntax = parsed.syntax().clone();
    let is_package = path.file_name().and_then(|name| name.to_str()) == Some("__init__.py");
    let module_name = module_name_from_path(root, path);
    let imports = collect_imports(&module_name, is_package, &syntax.body);
    let local_class_names = syntax
        .body
        .iter()
        .filter_map(|statement| match statement {
            Stmt::ClassDef(class_def) => Some(class_def.name.id.clone()),
            _ => None,
        })
        .collect();

    ModuleAnalysis {
        path: path.to_path_buf(),
        module_name,
        is_package,
        imports,
        local_class_names,
        body: syntax.body,
    }
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

    if components.last().is_some_and(|component| component == "__init__.py") {
        components.pop();
    } else if let Some(last) = components.last_mut() {
        if let Some(stem) = Path::new(last).file_stem().and_then(|stem| stem.to_str()) {
            *last = stem.to_string();
        }
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

fn apply_import_statement(
    statement: &Stmt,
    module_name: &str,
    is_package: bool,
    imports: &mut HashMap<String, String>,
) {
    match statement {
        Stmt::Import(import_stmt) => {
            for alias in &import_stmt.names {
                let local_name = alias
                    .asname
                    .as_ref()
                    .map(|asname| asname.id.clone())
                    .unwrap_or_else(|| alias.name.id.split('.').next().unwrap_or(alias.name.as_str()).to_string());
                imports.insert(local_name, alias.name.id.clone());
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
                    .map(|asname| asname.id.clone())
                    .unwrap_or_else(|| alias.name.id.clone());
                imports.insert(local_name, format!("{module}.{}", alias.name.id));
            }
        }
        _ => {}
    }
}

fn resolve_import_module(module_name: &str, is_package: bool, level: u32, imported_module: Option<&str>) -> Option<String> {
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
        package.split('.').map(ToOwned::to_owned).collect::<Vec<_>>()
    };

    for _ in 0..level.saturating_sub(1) {
        if parts.pop().is_none() {
            return None;
        }
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

                if !name.id.chars().all(|character| character.is_ascii_uppercase() || character == '_') {
                    continue;
                }

                if let Some(value) = expr_string_value(&assign.value) {
                    settings.insert(name.id.clone(), value);
                }
            }
            Stmt::AnnAssign(assign) => {
                let Expr::Name(name) = assign.target.as_ref() else {
                    continue;
                };

                if !name.id.chars().all(|character| character.is_ascii_uppercase() || character == '_') {
                    continue;
                }

                if let Some(value) = assign.value.as_ref().and_then(|value| expr_string_value(value)) {
                    settings.insert(name.id.clone(), value);
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
        .filter_map(|statement| extract_field(statement, module_name, &class_def.name.id, local_class_names, imports))
        .collect();

    RawClassInfo {
        id: ModelId::new(format!("{module_name}.{}", class_def.name.id)),
        module_name: module_name.to_string(),
        class_name: class_def.name.id.clone(),
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

            extract_field_from_value(target_name, &assign.value, module_name, class_name, local_class_names, imports)
        }
        Stmt::AnnAssign(assign) => {
            let target_name = match assign.target.as_ref() {
                Expr::Name(name) => name.id.as_str(),
                _ => return None,
            };

            assign
                .value
                .as_ref()
                .and_then(|value| extract_field_from_value(target_name, value, module_name, class_name, local_class_names, imports))
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
        call.arguments
            .args
            .first()
            .and_then(|target| extract_relation_target(target, module_name, class_name, local_class_names, imports))
    } else {
        None
    };

    Some(PendingField {
        name: target_name.to_string(),
        kind,
        relation_target,
        reverse_query_name: relation_query_name(kind, class_name, &call.arguments.keywords),
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
        Expr::StringLiteral(string_literal) => Some(PendingRelationTarget::StringLiteral(string_literal.value.to_str().to_string())),
        Expr::Attribute(attribute)
            if matches!(attribute.value.as_ref(), Expr::Name(name) if name.id.as_str() == "settings") =>
        {
            Some(PendingRelationTarget::SettingsKey(attribute.attr.id.clone()))
        }
        Expr::Name(_) | Expr::Attribute(_) => qualify_expr(expr, module_name, local_class_names, imports)
            .map(PendingRelationTarget::Qualified),
        _ => {
            let _ = class_name;
            None
        }
    }
}

fn relation_query_name(
    kind: FieldKind,
    class_name: &str,
    keywords: &[ast::Keyword],
) -> Option<String> {
    if !kind.is_relation() {
        return None;
    }

    let related_name = keyword_string_value(keywords, "related_name");
    if related_name.as_deref().is_some_and(|name| name.contains('+')) {
        return None;
    }

    keyword_string_value(keywords, "related_query_name")
        .or(related_name)
        .or_else(|| Some(class_name.to_lowercase()))
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
                Some(name.id.clone())
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
        Expr::Name(_) => resolve_model_reference_expr(expr, index, module_name, imports, local_bindings),
        Expr::Attribute(attribute) if attribute.attr.as_str() == "objects" => {
            infer_model_for_expression(&attribute.value, index, module_name, imports, local_bindings)
        }
        Expr::Attribute(_) => resolve_model_reference_expr(expr, index, module_name, imports, local_bindings),
        Expr::Call(call) => {
            let method = match call.func.as_ref() {
                Expr::Attribute(attribute) => attribute,
                _ => return None,
            };

            if QUERYSET_PRESERVING_METHODS.contains(&method.attr.as_str()) {
                infer_model_for_expression(&method.value, index, module_name, imports, local_bindings)
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
            .or_else(|| imports.get(name.id.as_str()).and_then(|qualified| index.resolve_qualified_model(qualified))),
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
        collect_scope_from_statement(statement, cursor, index, module_name, is_package, imports, bindings);
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
        Stmt::Assign(assign) if assign.range.end() <= cursor => {
            if assign.targets.len() == 1 {
                if let Expr::Name(name) = &assign.targets[0] {
                    if let Some(model_id) =
                        infer_model_for_expression(&assign.value, index, module_name, imports, bindings)
                    {
                        bindings.insert(name.id.clone(), model_id);
                    }
                }
            }
        }
        Stmt::AnnAssign(assign) if assign.range.end() <= cursor => {
            if let (Expr::Name(name), Some(value)) = (assign.target.as_ref(), assign.value.as_ref()) {
                if let Some(model_id) = infer_model_for_expression(value, index, module_name, imports, bindings) {
                    bindings.insert(name.id.clone(), model_id);
                }
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
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;
    use crate::document_store::DocumentStore;

    fn build_index(root: &Path) -> WorkspaceIndex {
        WorkspaceIndex::build(root, DjangoLspConfig::default(), &DocumentStore::default()).unwrap()
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
        assert_eq!(author.related_model.as_ref().unwrap().as_str(), "blog.models.Author");

        let author_model = index.model(author.related_model.as_ref().unwrap()).unwrap();
        let team = author_model.field("team").unwrap();
        assert_eq!(team.related_model.as_ref().unwrap().as_str(), "blog.models.Team");
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
            index.resolve_model_symbol_in_module(module, "Post").unwrap().as_str(),
            "blog.models.Blog"
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
            route.field("installer").unwrap().related_model.as_ref().unwrap().as_str(),
            "core.models.User"
        );
    }
}
