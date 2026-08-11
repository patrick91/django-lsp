use std::fs;
use std::process::Command;

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

#[test]
fn check_reports_diagnostics_and_uses_exit_status_one() {
    let directory = project(concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email)\n",
    ));

    let output = Command::new(env!("CARGO_BIN_EXE_django-lsp"))
        .arg("check")
        .arg("blog/views.py")
        .current_dir(directory.path())
        .output()
        .unwrap();

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

    let output = Command::new(env!("CARGO_BIN_EXE_django-lsp"))
        .arg("check")
        .current_dir(directory.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
