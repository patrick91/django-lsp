import { createDocsServer } from "@usecross/docs/ssr";

import { DocsPage } from "./components/DocsPage";
import Home from "./pages/Home";

createDocsServer({
  pages: {
    Home,
    "docs/DocsPage": DocsPage,
  },
  title: (title) => (title ? `${title} - django-lsp` : "django-lsp"),
});
