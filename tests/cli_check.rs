use std::fs;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

fn project(view_source: &str) -> tempfile::TempDir {
    let directory = tempdir().unwrap();
    let app = directory.path().join("blog");
    fs::create_dir_all(&app).unwrap();
    fs::write(app.join("__init__.py"), "").unwrap();
    fs::write(
        app.join("models.py"),
        concat!(
            "from django.db import models\n",
            "class Author(models.Model):\n",
            "    email = models.EmailField()\n",
            "class Blog(models.Model):\n",
            "    author = models.ForeignKey(Author, on_delete=models.CASCADE)\n",
        ),
    )
    .unwrap();
    fs::write(app.join("views.py"), view_source).unwrap();
    directory
}

fn check(directory: &tempfile::TempDir, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_django-lsp"));
    command
        .arg("check")
        .args(args)
        .current_dir(directory.path());
    command.output().unwrap()
}

#[test]
fn check_reports_diagnostics_and_uses_exit_status_one() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email)\n",
    ));

    let output = check(&directory, &["blog/views.py"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            "blog/views.py:3:16: warning DJ001: ",
            "Accessing `author` for each `Blog` may issue an extra query per row; ",
            "add `select_related(\"author\")` to the QuerySet.\n",
        )
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn check_succeeds_when_the_relation_is_loaded() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.select_related(\"author\"):\n",
        "    print(blog.author.email)\n",
    ));

    let output = check(&directory, &[]);

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn json_output_is_an_empty_array_when_the_check_passes() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.select_related(\"author\"):\n",
        "    print(blog.author.email)\n",
    ));

    let output = check(&directory, &["--format", "json"]);

    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        Value::Array(Vec::new())
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn invalid_check_options_use_exit_status_two() {
    let directory = project("");

    let output = check(&directory, &["--format", "sarif"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("invalid format `sarif`")
    );
}

#[test]
fn check_prints_stable_json() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email)\n",
    ));

    let output = check(&directory, &["--format", "json"]);

    assert_eq!(output.status.code(), Some(1));
    let findings: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(findings[0]["path"], "blog/views.py");
    assert_eq!(findings[0]["line"], 3);
    assert_eq!(findings[0]["column"], 16);
    assert_eq!(findings[0]["end_line"], 3);
    assert_eq!(findings[0]["end_column"], 22);
    assert_eq!(findings[0]["severity"], "warning");
    assert_eq!(findings[0]["code"], "DJ001");
    assert_eq!(findings[0]["suggestion"]["method"], "select_related");
    assert_eq!(findings[0]["suggestion"]["relation"], "author");
    assert!(output.stderr.is_empty());
}

#[test]
fn check_prints_github_actions_annotations() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email)\n",
    ));

    let output = check(&directory, &["--format=github"]);

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with(
        "::warning file=blog/views.py,line=3,col=16,endLine=3,endColumn=22,title=DJ001::"
    ));
    assert!(stdout.contains("add `select_related(\"author\")`"));
    assert!(output.stderr.is_empty());
}

#[test]
fn check_respects_inline_suppressions() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email)  # django-lsp: ignore[DJ001]\n",
    ));

    let output = check(&directory, &[]);

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn suppression_text_inside_a_string_is_not_a_directive() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email, '# django-lsp: ignore[DJ001]')\n",
    ));

    let output = check(&directory, &[]);

    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn check_respects_configured_test_exclusions() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.select_related('author'):\n",
        "    print(blog.author.email)\n",
    ));
    fs::write(
        directory.path().join("blog/tests.py"),
        concat!(
            "from .models import Blog\n",
            "for blog in Blog.objects.all():\n",
            "    print(blog.author.email)\n",
        ),
    )
    .unwrap();
    fs::write(
        directory.path().join("pyproject.toml"),
        "[tool.django-lsp]\nexclude = [\"**/tests.py\"]\n",
    )
    .unwrap();

    let output = check(&directory, &[]);

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
