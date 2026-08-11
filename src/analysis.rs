use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Arc, Mutex};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use salsa::Setter;

use crate::config::DjangoLspConfig;
use crate::diagnostic::{OrmDiagnostic, analyze_diagnostics};
use crate::error::{DjangoLspError, Result};
use crate::index::{
    CallableFacts, CallableIndex, ModuleAnalysis, ModuleFacts, WorkspaceIndex, analyze_source,
    callable_facts_from_analysis, facts_from_analysis,
};

const DEFAULT_EXCLUDES: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/.venv/**",
    "**/venv/**",
    "**/site-packages/**",
    "**/__pycache__/**",
    "**/migrations/**",
];

/// Mutable source text. Editor changes only advance the revision for this input.
#[salsa::input(debug)]
struct SourceFile {
    #[returns(deref)]
    path: PathBuf,
    #[returns(deref)]
    contents: String,
}

/// Project-wide inputs whose changes can affect module and model resolution.
#[salsa::input(debug)]
struct Project {
    #[returns(deref)]
    root: PathBuf,
    #[returns(ref)]
    config: DjangoLspConfig,
    #[returns(deref)]
    files: Vec<SourceFile>,
}

#[salsa::db]
trait Db: salsa::Database {}

#[salsa::tracked(returns(ref))]
fn parsed_module(db: &dyn Db, project: Project, file: SourceFile) -> ModuleAnalysis {
    analyze_source(project.root(db), file.path(db), file.contents(db))
}

/// Equality-comparable facts used by project-wide analysis.
///
/// Keeping function bodies out of this result lets Salsa backdate ordinary implementation edits:
/// the file is reparsed, but model schemas and other project-wide consumers remain valid.
#[salsa::tracked(returns(ref))]
fn module_facts(db: &dyn Db, project: Project, file: SourceFile) -> ModuleFacts {
    facts_from_analysis(parsed_module(db, project, file))
}

#[salsa::tracked(returns(ref))]
fn callable_facts(db: &dyn Db, project: Project, file: SourceFile) -> CallableFacts {
    callable_facts_from_analysis(parsed_module(db, project, file))
}

#[salsa::tracked(returns(ref))]
fn project_index(db: &dyn Db, project: Project) -> WorkspaceIndex {
    let facts = project
        .files(db)
        .iter()
        .map(|file| module_facts(db, project, *file))
        .collect::<Vec<_>>();
    WorkspaceIndex::from_facts(
        project.root(db).to_path_buf(),
        project.config(db).clone(),
        &facts,
    )
}

#[salsa::tracked(returns(ref))]
fn project_callable_index(db: &dyn Db, project: Project) -> CallableIndex {
    let facts = project
        .files(db)
        .iter()
        .map(|file| callable_facts(db, project, *file))
        .collect::<Vec<_>>();
    CallableIndex::from_facts(&facts)
}

#[salsa::tracked(returns(ref))]
fn file_diagnostics(db: &dyn Db, project: Project, file: SourceFile) -> Vec<OrmDiagnostic> {
    analyze_diagnostics(
        project_index(db, project),
        project_callable_index(db, project),
        parsed_module(db, project, file),
    )
}

/// Incremental database shared by editor features and future command-line diagnostics.
#[salsa::db]
#[derive(Clone)]
pub struct AnalysisDatabase {
    storage: salsa::Storage<Self>,
    project: Option<Project>,
    files: HashMap<PathBuf, SourceFile>,
    #[cfg(test)]
    events: Arc<Mutex<Option<Vec<String>>>>,
}

impl Default for AnalysisDatabase {
    fn default() -> Self {
        #[cfg(test)]
        let events = Arc::<Mutex<Option<Vec<String>>>>::default();
        #[cfg(test)]
        let storage = salsa::Storage::new(Some(Box::new({
            let events = Arc::clone(&events);
            move |event| {
                if let salsa::EventKind::WillExecute { .. } = event.kind
                    && let Some(events) = &mut *events.lock().expect("event log poisoned")
                {
                    events.push(format!("{event:?}"));
                }
            }
        })));
        #[cfg(not(test))]
        let storage = salsa::Storage::new(None);

        Self {
            storage,
            project: None,
            files: HashMap::new(),
            #[cfg(test)]
            events,
        }
    }
}

impl fmt::Debug for AnalysisDatabase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnalysisDatabase")
            .field("root", &self.project.map(|project| project.root(self)))
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

#[salsa::db]
impl salsa::Database for AnalysisDatabase {}

#[salsa::db]
impl Db for AnalysisDatabase {}

impl AnalysisDatabase {
    pub fn build(workspace_root: &Path, config: DjangoLspConfig) -> Result<Self> {
        let root = config.effective_root(workspace_root);
        let matcher = PathMatcher::new(&root, &config)?;
        let mut walker = WalkBuilder::new(&root);
        walker.hidden(false);
        walker.git_ignore(true);
        walker.git_global(true);
        walker.git_exclude(true);
        walker.parents(true);

        let mut database = Self::default();
        let mut files = Vec::new();
        for entry in walker.build() {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
                || !matcher.matches(path)
            {
                continue;
            }

            let contents = fs::read_to_string(path).ok();
            let Some(contents) = contents else {
                continue;
            };
            let path = path.to_path_buf();
            let file = SourceFile::new(&database, path.clone(), contents);
            database.files.insert(path, file);
            files.push(file);
        }
        files.sort_unstable_by(|left, right| left.path(&database).cmp(right.path(&database)));
        database.project = Some(Project::new(&database, root, config, files));
        Ok(database)
    }

    pub fn empty(workspace_root: PathBuf, config: DjangoLspConfig) -> Self {
        let mut database = Self::default();
        let root = config.effective_root(&workspace_root);
        database.project = Some(Project::new(&database, root, config, Vec::new()));
        database
    }

    pub fn index(&self) -> &WorkspaceIndex {
        project_index(self, self.project())
    }

    pub fn analysis_for_path(&self, path: &Path) -> Option<&ModuleAnalysis> {
        let file = *self.files.get(path)?;
        Some(parsed_module(self, self.project(), file))
    }

    pub fn source_for_path(&self, path: &Path) -> Option<&str> {
        let file = *self.files.get(path)?;
        Some(file.contents(self))
    }

    pub fn diagnostics_for_path(&self, path: &Path) -> Option<&[OrmDiagnostic]> {
        let file = *self.files.get(path)?;
        Some(file_diagnostics(self, self.project(), file))
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.project()
            .files(self)
            .iter()
            .map(|file| file.path(self).to_path_buf())
            .collect()
    }

    pub fn sync_path(&mut self, path: PathBuf, contents: Option<String>) -> Result<bool> {
        if let Some(file) = self.files.get(&path).copied() {
            if let Some(contents) = contents {
                file.set_contents(self).to(contents);
            } else {
                self.remove_file(&path);
            }
            return Ok(true);
        }

        let Some(contents) = contents else {
            return Ok(false);
        };
        let project = self.project();
        if !PathMatcher::new(project.root(self), project.config(self))?.matches(&path) {
            return Ok(false);
        }

        let file = SourceFile::new(self, path.clone(), contents);
        self.files.insert(path, file);
        let mut files = project.files(self).to_vec();
        files.push(file);
        files.sort_unstable_by(|left, right| left.path(self).cmp(right.path(self)));
        project.set_files(self).to(files);
        Ok(true)
    }

    pub fn sync_path_from_disk(&mut self, path: PathBuf) -> Result<bool> {
        let contents = fs::read_to_string(&path).ok();
        self.sync_path(path, contents)
    }

    pub fn analyzed_file_count(&self) -> usize {
        self.project().files(self).len()
    }

    #[cfg(test)]
    pub(crate) fn enable_event_logging(&self) {
        *self.events.lock().expect("event log poisoned") = Some(Vec::new());
    }

    #[cfg(test)]
    pub(crate) fn take_events(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("event log poisoned")
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    fn project(&self) -> Project {
        self.project.expect("analysis database is not initialized")
    }

    fn remove_file(&mut self, path: &Path) {
        let Some(file) = self.files.remove(path) else {
            return;
        };
        let project = self.project();
        let files = project
            .files(self)
            .iter()
            .copied()
            .filter(|candidate| *candidate != file)
            .collect();
        project.set_files(self).to(files);
    }
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
            Some(build_globset(&config.include)?)
        };

        let mut exclude_patterns = DEFAULT_EXCLUDES
            .iter()
            .map(|pattern| pattern.to_string())
            .collect::<Vec<_>>();
        exclude_patterns.extend(config.exclude.iter().cloned());

        Ok(Self {
            root: root.to_path_buf(),
            include,
            exclude: build_globset(&exclude_patterns)?,
        })
    }

    fn matches(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root)
            || path.extension().and_then(|extension| extension.to_str()) != Some("py")
        {
            return false;
        }

        let relative = path
            .strip_prefix(&self.root)
            .expect("path was checked against the project root");
        if self.exclude.is_match(relative) {
            return false;
        }

        self.include
            .as_ref()
            .is_none_or(|include| include.is_match(relative))
    }
}

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern).map_err(|source| DjangoLspError::glob(pattern.clone(), source))?,
        );
        if !pattern.starts_with("**/") && !pattern.starts_with('.') && !pattern.contains('/') {
            let scoped = format!("**/{pattern}");
            builder.add(
                Glob::new(&scoped)
                    .map_err(|source| DjangoLspError::glob(scoped.clone(), source))?,
            );
        }
    }
    builder
        .build()
        .map_err(|source| DjangoLspError::glob("globset".to_string(), source))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn reuses_queries_and_backdates_unchanged_project_facts() {
        let directory = tempdir().unwrap();
        let app = directory.path().join("blog");
        let models = app.join("models.py");
        let views = app.join("views.py");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            &models,
            "from django.db import models\nclass Blog(models.Model):\n    title = models.CharField()\n",
        )
        .unwrap();
        fs::write(
            &views,
            "from .models import Blog\nBlog.objects.filter(title='first')\n",
        )
        .unwrap();

        let mut database =
            AnalysisDatabase::build(directory.path(), DjangoLspConfig::default()).unwrap();
        database.enable_event_logging();

        assert!(
            database
                .index()
                .model(&crate::index::ModelId::new("blog.models.Blog"))
                .is_some()
        );
        assert!(!database.take_events().is_empty());

        let _ = database.index();
        assert!(database.take_events().is_empty());

        database
            .sync_path(
                views,
                Some("from .models import Blog\nBlog.objects.filter(title='second')\n".to_string()),
            )
            .unwrap();
        let _ = database.index();
        assert_eq!(
            database.take_events().len(),
            2,
            "only the changed file parse and its project-facts query should execute"
        );

        database
            .sync_path(
                models,
                Some(
                    "from django.db import models\nclass Blog(models.Model):\n    title = models.CharField()\n    summary = models.TextField()\n"
                        .to_string(),
                ),
            )
            .unwrap();
        assert!(
            database
                .index()
                .model(&crate::index::ModelId::new("blog.models.Blog"))
                .unwrap()
                .field("summary")
                .is_some()
        );
        assert_eq!(
            database.take_events().len(),
            3,
            "a model change should also rebuild the dependent project index"
        );
    }
}
