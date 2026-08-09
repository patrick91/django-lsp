import { Link } from "@inertiajs/react";
import { MobileMenuButton, ThemeToggle } from "@usecross/docs";

interface SiteHeaderProps {
  githubUrl?: string;
  isMenuOpen: boolean;
  navLinks?: Array<{ label: string; href: string }>;
  onToggleMenu: () => void;
}

interface SiteFooterProps {
  githubUrl?: string;
  navLinks?: Array<{ label: string; href: string }>;
}

export function Brand() {
  return (
    <Link
      href="/"
      aria-label="django-lsp homepage"
      className="font-heading text-lg font-semibold tracking-tight text-gray-900 dark:text-white"
    >
      django<span className="text-primary-500 dark:text-primary-400">-lsp</span>
    </Link>
  );
}

function GitHubIcon() {
  return (
    <svg className="size-6" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <path
        fillRule="evenodd"
        d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z"
        clipRule="evenodd"
      />
    </svg>
  );
}

export function SiteHeader({
  githubUrl,
  isMenuOpen,
  navLinks = [],
  onToggleMenu,
}: SiteHeaderProps) {
  return (
    <nav
      aria-label="Primary navigation"
      className="fixed z-50 w-full border-b border-gray-200 bg-white/95 backdrop-blur-sm dark:border-gray-800 dark:bg-[#0f0f0f]/95"
    >
      <div className="px-4 lg:px-10">
        <div className="flex h-16 items-center justify-between">
          <div className="flex items-center gap-2">
            <MobileMenuButton onClick={onToggleMenu} isOpen={isMenuOpen} />
            <Brand />
          </div>
          <div className="flex items-center gap-6">
            <div className="-mr-2">
              <ThemeToggle size="sm" />
            </div>
            {navLinks.map((link) => (
              <Link
                key={link.href}
                href={link.href}
                className="text-base font-medium text-gray-700 hover:text-primary-600 sm:text-sm dark:text-gray-300 dark:hover:text-primary-400"
              >
                {link.label}
              </Link>
            ))}
            {githubUrl && (
              <a
                href={githubUrl}
                target="_blank"
                rel="noopener noreferrer"
                aria-label="django-lsp on GitHub"
                className="text-gray-700 hover:text-primary-600 dark:text-gray-300 dark:hover:text-primary-400"
              >
                <GitHubIcon />
              </a>
            )}
          </div>
        </div>
      </div>
    </nav>
  );
}

export function SiteFooter({ githubUrl, navLinks = [] }: SiteFooterProps) {
  return (
    <footer className="border-t border-gray-200 bg-white py-8 dark:border-gray-800 dark:bg-[#0f0f0f]">
      <div className="flex flex-col items-center justify-between gap-6 px-4 md:flex-row lg:px-10">
        <Brand />
        <nav aria-label="Footer navigation" className="flex items-center gap-8">
          {navLinks.map((link) => (
            <Link
              key={link.href}
              href={link.href}
              className="text-base font-normal text-gray-600 hover:text-black sm:text-sm dark:text-gray-300 dark:hover:text-white"
            >
              {link.label}
            </Link>
          ))}
          {githubUrl && (
            <a
              href={githubUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="text-base font-normal text-gray-600 hover:text-black sm:text-sm dark:text-gray-300 dark:hover:text-white"
            >
              GitHub
            </a>
          )}
        </nav>
      </div>
    </footer>
  );
}
