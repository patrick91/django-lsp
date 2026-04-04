use std::collections::{HashMap, HashSet};
use std::path::Path;

use ruff_python_parser::parse_expression;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, CompletionTextEdit, Range, TextEdit};

use crate::index::{
    analyze_source, collect_visible_scope, infer_model_for_expression, FieldInfo, ModelId, WorkspaceIndex,
};

const QUERY_METHODS: &[&str] = &["filter", "exclude", "get"];
const RELATION_LOOKUPS: &[&str] = &["exact", "in", "isnull"];
const MAX_COMPLETION_RELATION_DEPTH: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    pub replace_start: usize,
    pub replace_end: usize,
    pub token: String,
    pub base_expression: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub label: String,
    pub insert_text: String,
    pub filter_text: String,
    pub detail: String,
    pub kind: CompletionItemKind,
    sort_group: u8,
    sort_rank: usize,
}

pub fn complete(
    index: &WorkspaceIndex,
    path: &Path,
    source: &str,
    cursor_offset: usize,
) -> Vec<CompletionCandidate> {
    let Some(request) = extract_completion_request(source, cursor_offset) else {
        return Vec::new();
    };

    if !QUERY_METHODS.contains(&request.method.as_str()) {
        return Vec::new();
    }

    let analysis = analyze_source(index.root(), path, source);
    let mut imports = analysis.imports.clone();
    let mut bindings = HashMap::new();
    collect_visible_scope(
        &analysis.body,
        cursor_offset,
        index,
        &analysis.module_name,
        analysis.is_package,
        &mut imports,
        &mut bindings,
    );

    let Ok(parsed) = parse_expression(&request.base_expression) else {
        return Vec::new();
    };

    let Some(model_id) = infer_model_for_expression(
        &parsed.syntax().body,
        index,
        &analysis.module_name,
        &imports,
        &bindings,
    ) else {
        return Vec::new();
    };

    build_candidates(index, &model_id, &request.token)
}

pub fn complete_lsp_items(
    index: &WorkspaceIndex,
    path: &Path,
    source: &str,
    cursor_offset: usize,
) -> Vec<CompletionItem> {
    let Some(request) = extract_completion_request(source, cursor_offset) else {
        return Vec::new();
    };

    let range = offsets_to_range(source, request.replace_start, request.replace_end);
    complete(index, path, source, cursor_offset)
        .into_iter()
        .enumerate()
        .map(|(order, candidate)| CompletionItem {
            label: candidate.filter_text.clone(),
            kind: Some(candidate.kind),
            detail: Some(candidate.detail),
            filter_text: Some(candidate.filter_text),
            sort_text: Some(format!("{order:04}")),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range,
                new_text: candidate.insert_text,
            })),
            ..CompletionItem::default()
        })
        .collect()
}

fn build_candidates(index: &WorkspaceIndex, root_model_id: &ModelId, token: &str) -> Vec<CompletionCandidate> {
    let Some(root_model) = index.model(root_model_id) else {
        return Vec::new();
    };

    let mut segments = token.split("__").collect::<Vec<_>>();
    if segments.is_empty() {
        segments.push("");
    }

    let _prefix = segments.pop().unwrap_or_default();
    let path_segments = segments;
    let mut current_model = root_model;
    let mut chain_parts = Vec::new();
    let mut last_field: Option<&FieldInfo> = None;

    for (index_position, segment) in path_segments.iter().enumerate() {
        if segment.is_empty() {
            return Vec::new();
        }

        let Some(field) = current_model.field(segment) else {
            return Vec::new();
        };
        chain_parts.push((*segment).to_string());
        last_field = Some(field);

        if let Some(related_model) = &field.related_model {
            let Some(next_model) = index.model(related_model) else {
                return Vec::new();
            };
            current_model = next_model;
        } else if index_position + 1 != path_segments.len() {
            return Vec::new();
        }
    }

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    let context_chain = chain_parts.join("__");

    match last_field {
        Some(field) if field.related_model.is_none() => {
            add_lookup_candidates(
                &mut candidates,
                &mut seen,
                &context_chain,
                field.supported_lookups,
                token,
                field.name.as_str(),
                false,
                &context_chain,
            );
        }
        Some(field) if field.related_model.is_some() => {
            add_relation_lookup_candidates(
                &mut candidates,
                &mut seen,
                &context_chain,
                token,
                field.name.as_str(),
                &context_chain,
            );
            let mut visited = HashSet::from([current_model.id.clone()]);
            add_descendant_candidates(
                &mut candidates,
                &mut seen,
                index,
                current_model,
                token,
                &context_chain,
                &context_chain,
                0,
                &mut visited,
            );
        }
        None => {
            let mut visited = HashSet::from([current_model.id.clone()]);
            add_descendant_candidates(
                &mut candidates,
                &mut seen,
                index,
                current_model,
                token,
                "",
                "",
                0,
                &mut visited,
            );
        }
        Some(_) => {}
    }

    candidates.sort_by(|left, right| {
        left.sort_group
            .cmp(&right.sort_group)
            .then_with(|| left.sort_rank.cmp(&right.sort_rank))
            .then_with(|| left.filter_text.len().cmp(&right.filter_text.len()))
            .then_with(|| left.filter_text.cmp(&right.filter_text))
    });
    candidates
}

fn add_descendant_candidates(
    candidates: &mut Vec<CompletionCandidate>,
    seen: &mut HashSet<String>,
    index: &WorkspaceIndex,
    model: &crate::index::ModelInfo,
    typed_token: &str,
    context_chain: &str,
    prefix_chain: &str,
    relation_depth: usize,
    visited: &mut HashSet<ModelId>,
) {
    for field in &model.fields {
        let full_path = join_chain(prefix_chain, &field.name);
        add_field_candidate(candidates, seen, model, &field.name, &full_path, typed_token, context_chain);

        if field.related_model.is_some() {
            add_relation_lookup_candidates(
                candidates,
                seen,
                &full_path,
                typed_token,
                field.name.as_str(),
                context_chain,
            );

            if relation_depth >= MAX_COMPLETION_RELATION_DEPTH {
                continue;
            }

            let Some(related_model) = field.related_model.as_ref() else {
                continue;
            };
            if !visited.insert(related_model.clone()) {
                continue;
            }

            if let Some(next_model) = index.model(related_model) {
                add_descendant_candidates(
                    candidates,
                    seen,
                    index,
                    next_model,
                    typed_token,
                    context_chain,
                    &full_path,
                    relation_depth + 1,
                    visited,
                );
            }
            visited.remove(related_model);
        } else {
            add_lookup_candidates(
                candidates,
                seen,
                &full_path,
                field.supported_lookups,
                typed_token,
                field.name.as_str(),
                false,
                context_chain,
            );
        }
    }
}

fn add_field_candidate(
    candidates: &mut Vec<CompletionCandidate>,
    seen: &mut HashSet<String>,
    model: &crate::index::ModelInfo,
    field_name: &str,
    full_path: &str,
    typed_token: &str,
    context_chain: &str,
) {
    if !full_path.starts_with(typed_token) {
        return;
    }

    if seen.insert(full_path.to_string()) {
        candidates.push(CompletionCandidate {
            label: field_name.to_string(),
            insert_text: completion_insert_text(full_path, context_chain),
            filter_text: full_path.to_string(),
            detail: format!("field on {}", model.class_name),
            kind: CompletionItemKind::FIELD,
            sort_group: 0,
            sort_rank: path_depth(full_path) * 1000 + full_path.len(),
        });
    }
}

fn add_relation_lookup_candidates(
    candidates: &mut Vec<CompletionCandidate>,
    seen: &mut HashSet<String>,
    prefix_chain: &str,
    typed_token: &str,
    field_name: &str,
    context_chain: &str,
) {
    add_lookup_candidates(
        candidates,
        seen,
        prefix_chain,
        RELATION_LOOKUPS,
        typed_token,
        field_name,
        true,
        context_chain,
    );
}

fn add_lookup_candidates(
    candidates: &mut Vec<CompletionCandidate>,
    seen: &mut HashSet<String>,
    prefix_chain: &str,
    supported_lookups: &[&str],
    typed_token: &str,
    field_name: &str,
    relation_lookup: bool,
    context_chain: &str,
) {
    for (rank, lookup) in supported_lookups.iter().enumerate() {
        let filter_text = format!("{prefix_chain}__{lookup}");
        if !filter_text.starts_with(typed_token) {
            continue;
        }

        let label = (*lookup).to_string();

        if seen.insert(filter_text.clone()) {
            candidates.push(CompletionCandidate {
                label,
                insert_text: completion_insert_text(&filter_text, context_chain),
                filter_text,
                detail: if relation_lookup {
                    format!("relation lookup on {field_name}")
                } else {
                    format!("lookup on {field_name}")
                },
                kind: CompletionItemKind::OPERATOR,
                sort_group: 1,
                sort_rank: path_depth(prefix_chain) * 1000 + rank,
            });
        }
    }
}

fn join_chain(prefix_chain: &str, segment: &str) -> String {
    if prefix_chain.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix_chain}__{segment}")
    }
}

fn completion_insert_text(full_path: &str, context_chain: &str) -> String {
    if context_chain.is_empty() {
        full_path.to_string()
    } else {
        full_path
            .strip_prefix(&format!("{context_chain}__"))
            .unwrap_or(full_path)
            .to_string()
    }
}

fn path_depth(path: &str) -> usize {
    path.matches("__").count()
}

pub fn extract_completion_request(source: &str, cursor_offset: usize) -> Option<CompletionRequest> {
    if cursor_offset > source.len() {
        return None;
    }

    let replace_start = scan_identifier_start(source, cursor_offset);
    let token = source.get(replace_start..cursor_offset)?.to_string();
    let segment_start = token
        .rfind("__")
        .map(|index| replace_start + index + 2)
        .unwrap_or(replace_start);
    let significant_left = skip_whitespace_left(source, replace_start);
    let previous_char = significant_left.and_then(|index| source[..index].chars().next_back());
    if previous_char == Some('=') {
        return None;
    }

    let open_paren = find_enclosing_call_open(source, replace_start)?;
    let receiver = extract_receiver(source, open_paren)?;
    let (base_expression, method) = split_receiver(&receiver)?;
    if !QUERY_METHODS.contains(&method.as_str()) {
        return None;
    }

    Some(CompletionRequest {
        replace_start: segment_start,
        replace_end: cursor_offset,
        token,
        base_expression,
        method,
    })
}

fn scan_identifier_start(source: &str, cursor_offset: usize) -> usize {
    let mut index = cursor_offset;
    while index > 0 {
        let ch = source[..index].chars().next_back().unwrap();
        if ch.is_ascii_alphanumeric() || ch == '_' {
            index -= ch.len_utf8();
        } else {
            break;
        }
    }
    index
}

fn skip_whitespace_left(source: &str, index: usize) -> Option<usize> {
    let mut current = index;
    while current > 0 {
        let ch = source[..current].chars().next_back().unwrap();
        if ch.is_whitespace() {
            current -= ch.len_utf8();
        } else {
            break;
        }
    }

    (current > 0).then_some(current)
}

fn find_enclosing_call_open(source: &str, index: usize) -> Option<usize> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;

    for (offset, ch) in source[..index].char_indices().rev() {
        match ch {
            ')' => parens += 1,
            ']' => brackets += 1,
            '}' => braces += 1,
            '(' => {
                if parens == 0 && brackets == 0 && braces == 0 {
                    return Some(offset);
                }
                parens = parens.saturating_sub(1);
            }
            '[' => brackets = brackets.saturating_sub(1),
            '{' => braces = braces.saturating_sub(1),
            _ => {}
        }
    }

    None
}

fn extract_receiver(source: &str, open_paren: usize) -> Option<String> {
    let end = skip_whitespace_left(source, open_paren).unwrap_or(open_paren);
    let mut start = end;
    let mut parens = 0usize;
    let mut brackets = 0usize;

    for (offset, ch) in source[..end].char_indices().rev() {
        match ch {
            ')' => {
                parens += 1;
                start = offset;
            }
            ']' => {
                brackets += 1;
                start = offset;
            }
            '(' => {
                if parens == 0 {
                    break;
                }
                parens -= 1;
                start = offset;
            }
            '[' => {
                if brackets == 0 {
                    break;
                }
                brackets -= 1;
                start = offset;
            }
            '.' | '_' if parens == 0 && brackets == 0 => {
                start = offset;
            }
            c if c.is_ascii_alphanumeric() => {
                start = offset;
            }
            c if c.is_whitespace() && parens == 0 && brackets == 0 => break,
            _ if parens == 0 && brackets == 0 => break,
            _ => start = offset,
        }
    }

    let receiver = source.get(start..end)?.trim();
    (!receiver.is_empty()).then(|| receiver.to_string())
}

fn split_receiver(receiver: &str) -> Option<(String, String)> {
    let (base, method) = receiver.rsplit_once('.')?;
    Some((base.trim().to_string(), method.trim().to_string()))
}

fn offsets_to_range(source: &str, start: usize, end: usize) -> Range {
    Range {
        start: offset_to_position(source, start),
        end: offset_to_position(source, end),
    }
}

fn offset_to_position(source: &str, offset: usize) -> tower_lsp::lsp_types::Position {
    let mut line = 0u32;
    let mut column = 0u32;
    let mut seen = 0usize;

    for ch in source.chars() {
        if seen >= offset {
            break;
        }
        let len = ch.len_utf8();
        if seen + len > offset {
            break;
        }

        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += ch.len_utf16() as u32;
        }
        seen += len;
    }

    tower_lsp::lsp_types::Position::new(line, column)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::config::DjangoLspConfig;
    use crate::document_store::DocumentStore;
    use crate::index::WorkspaceIndex;

    fn fixture_index(files: &[(&str, &str)]) -> (tempfile::TempDir, WorkspaceIndex) {
        let dir = tempdir().unwrap();
        for (relative, contents) in files {
            let path = dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, contents).unwrap();
        }

        let index = WorkspaceIndex::build(dir.path(), DjangoLspConfig::default(), &DocumentStore::default()).unwrap();
        (dir, index)
    }

    fn labels(items: Vec<CompletionCandidate>) -> Vec<String> {
        items.into_iter().map(|item| item.label).collect()
    }

    #[test]
    fn completes_direct_fields() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Author(models.Model):
    email = models.EmailField()

class Blog(models.Model):
    title = models.CharField(max_length=255)
    slug = models.SlugField()
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(ti)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("ti)").unwrap() + 2;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        let labels = labels(items);
        assert!(labels.contains(&"title".to_string()));
        assert!(labels.contains(&"icontains".to_string()));
    }

    #[test]
    fn completes_relation_fields() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Team(models.Model):
    name = models.CharField(max_length=64)

class Author(models.Model):
    email = models.EmailField()
    team = models.ForeignKey(Team, on_delete=models.CASCADE)

class Blog(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(author__)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("author__)").unwrap() + "author__".len();
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        let labels = labels(items);
        assert!(labels.contains(&"email".to_string()));
        assert!(labels.contains(&"team".to_string()));
        assert!(labels.contains(&"isnull".to_string()));
    }

    #[test]
    fn completes_deep_relation_lookups() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Team(models.Model):
    name = models.CharField(max_length=64)

class Author(models.Model):
    team = models.ForeignKey(Team, on_delete=models.CASCADE)

class Blog(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(author__team__name__i)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("__i)").unwrap() + 3;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        let labels = labels(items);
        assert!(labels.contains(&"icontains".to_string()));
        assert!(labels.contains(&"iexact".to_string()));
    }

    #[test]
    fn completes_root_prefix_with_descendant_paths() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Team(models.Model):
    name = models.CharField(max_length=64)

class Author(models.Model):
    email = models.EmailField()
    team = models.ForeignKey(Team, on_delete=models.CASCADE)

class Blog(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(au)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("au)").unwrap() + 2;
        let items = complete_lsp_items(&index, &dir.path().join("blog/views.py"), &source, cursor);
        let labels = items.into_iter().map(|item| item.label).collect::<Vec<_>>();

        assert!(labels.contains(&"author".to_string()));
        assert!(labels.contains(&"author__email".to_string()));
        assert!(labels.contains(&"author__team".to_string()));
        assert!(labels.contains(&"author__team__name".to_string()));
        assert!(labels.contains(&"author__email__icontains".to_string()));
    }

    #[test]
    fn descendant_paths_insert_only_missing_suffix() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Team(models.Model):
    name = models.CharField(max_length=64)

class Author(models.Model):
    team = models.ForeignKey(Team, on_delete=models.CASCADE)

class Blog(models.Model):
    author = models.ForeignKey(Author, on_delete=models.CASCADE)
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(author__te)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("__te)").unwrap() + 4;
        let items = complete_lsp_items(&index, &dir.path().join("blog/views.py"), &source, cursor);
        let team_name = items
            .into_iter()
            .find(|item| item.label == "author__team__name")
            .unwrap();

        let edit = match team_name.text_edit.unwrap() {
            CompletionTextEdit::Edit(edit) => edit,
            CompletionTextEdit::InsertAndReplace(_) => panic!("unexpected insert-and-replace edit"),
        };
        assert_eq!(edit.new_text, "team__name");
    }

    #[test]
    fn completes_explicit_reverse_relations() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Blog(models.Model):
    title = models.CharField(max_length=255)

class Entry(models.Model):
    blog = models.ForeignKey(Blog, on_delete=models.CASCADE, related_name="entries")
    headline = models.CharField(max_length=255)
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(ent)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("ent)").unwrap() + 3;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        assert!(labels(items).contains(&"entries".to_string()));
    }

    #[test]
    fn completes_default_reverse_relation_queries() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Team(models.Model):
    name = models.CharField(max_length=64)

class Author(models.Model):
    team = models.ForeignKey(Team, on_delete=models.CASCADE)
    email = models.EmailField()
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Team

Team.objects.filter(auth)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("auth)").unwrap() + 4;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        assert!(labels(items).contains(&"author".to_string()));
    }

    #[test]
    fn uses_unsaved_buffers_for_model_fields() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("blog")).unwrap();
        fs::write(dir.path().join("blog/models.py"), "from django.db import models\n\nclass Blog(models.Model):\n    title = models.CharField(max_length=255)\n").unwrap();
        fs::write(
            dir.path().join("blog/views.py"),
            "from .models import Blog\n\nBlog.objects.filter(ne)\n",
        )
        .unwrap();

        let mut documents = DocumentStore::default();
        documents.open(
            dir.path().join("blog/models.py"),
            2,
            "from django.db import models\n\nclass Blog(models.Model):\n    title = models.CharField(max_length=255)\n    new_field = models.IntegerField()\n".to_string(),
        );

        let index = WorkspaceIndex::build(dir.path(), DjangoLspConfig::default(), &documents).unwrap();
        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("ne)").unwrap() + 2;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        assert!(labels(items).contains(&"new_field".to_string()));
    }

    #[test]
    fn completes_auth_user_model_relations() {
        let (dir, index) = fixture_index(&[
            (
                "config/settings.py",
                "AUTH_USER_MODEL = 'core.User'\n",
            ),
            (
                "core/models.py",
                r#"
from django.conf import settings
from django.contrib.auth.models import AbstractUser
from django.db import models

class User(AbstractUser):
    email = models.EmailField(unique=True)

class DailyPlanRoute(models.Model):
    lead_installer = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.CASCADE,
        related_name="led_routes",
    )
"#,
            ),
            (
                "core/views.py",
                r#"
from .models import DailyPlanRoute

DailyPlanRoute.objects.filter(lead_installer__ema)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("core/views.py")).unwrap();
        let cursor = source.find("__ema)").unwrap() + 5;
        let items = complete(&index, &dir.path().join("core/views.py"), &source, cursor);
        assert!(labels(items).contains(&"email".to_string()));
    }

    #[test]
    fn completes_function_local_from_imports() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Blog(models.Model):
    title = models.CharField(max_length=255)
"#,
            ),
            (
                "blog/views.py",
                r#"
def run():
    from .models import Blog
    Blog.objects.filter(ti)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("ti)").unwrap() + 2;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        assert!(labels(items).contains(&"title".to_string()));
    }

    #[test]
    fn completes_function_local_module_alias_imports() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Blog(models.Model):
    title = models.CharField(max_length=255)
"#,
            ),
            (
                "blog/views.py",
                r#"
def run():
    import blog.models as blog_models
    blog_models.Blog.objects.filter(ti)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("ti)").unwrap() + 2;
        let items = complete(&index, &dir.path().join("blog/views.py"), &source, cursor);
        assert!(labels(items).contains(&"title".to_string()));
    }

    #[test]
    fn lsp_items_keep_full_filter_text_for_lookup_suffixes() {
        let (dir, index) = fixture_index(&[
            (
                "blog/models.py",
                r#"
from django.db import models

class Blog(models.Model):
    notes = models.TextField()
"#,
            ),
            (
                "blog/views.py",
                r#"
from .models import Blog

Blog.objects.filter(notes__)
"#,
            ),
        ]);

        let source = fs::read_to_string(dir.path().join("blog/views.py")).unwrap();
        let cursor = source.find("notes__)").unwrap() + "notes__".len();
        let items = complete_lsp_items(&index, &dir.path().join("blog/views.py"), &source, cursor);
        let exact = items.into_iter().find(|item| item.label == "notes__exact").unwrap();

        assert_eq!(exact.filter_text.as_deref(), Some("notes__exact"));
        let edit = match exact.text_edit.unwrap() {
            CompletionTextEdit::Edit(edit) => edit,
            CompletionTextEdit::InsertAndReplace(_) => panic!("unexpected insert-and-replace edit"),
        };
        assert_eq!(edit.new_text, "exact");
    }
}
