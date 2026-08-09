import { createDocsApp } from "@usecross/docs";

import { DocsPage } from "./components/DocsPage";
import Home from "./pages/Home";
import "./styles.css";

createDocsApp({
  pages: {
    Home,
    "docs/DocsPage": DocsPage,
  },
  title: (title) => (title ? `${title} - django-lsp` : "django-lsp"),
});
