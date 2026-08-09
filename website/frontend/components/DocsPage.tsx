import { DocsLayout, DocsPage as CrossDocsPage, Markdown } from "@usecross/docs";
import type { ComponentProps } from "react";

import { AutocompleteDemo } from "./AutocompleteDemo";
import { SiteFooter, SiteHeader } from "./SiteChrome";

type DocsPageProps = ComponentProps<typeof CrossDocsPage>;

export function DocsPage({ content, ...props }: DocsPageProps) {
  const navLinks = props.navLinks ?? [{ label: "Docs", href: "/docs/" }];

  return (
    <DocsLayout
      {...props}
      title={content?.title ?? ""}
      description={content?.description}
      toc={content?.toc}
      header={({ mobileMenuOpen, toggleMobileMenu }) => (
        <SiteHeader
          githubUrl={props.githubUrl}
          isMenuOpen={mobileMenuOpen}
          navLinks={navLinks}
          onToggleMenu={toggleMobileMenu}
        />
      )}
      footer={<SiteFooter githubUrl={props.githubUrl} navLinks={navLinks} />}
    >
      <Markdown content={content?.body ?? ""} components={{ AutocompleteDemo }} />
    </DocsLayout>
  );
}
