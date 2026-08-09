import {
  HomeCTA,
  HomeFeatures,
  HomeHeader,
  HomeHero,
  HomePage,
  type HomePageProps,
} from "@usecross/docs";

import { AutocompleteDemo } from "../components/AutocompleteDemo";
import { Brand, SiteFooter } from "../components/SiteChrome";

export default function Home(props: HomePageProps) {
  const navLinks = props.navLinks ?? [{ label: "Docs", href: "/docs/" }];

  return (
    <HomePage {...props} navLinks={navLinks}>
      <HomeHeader renderLogo={() => <Brand />} />
      <HomeHero />
      <HomeFeatures />
      <section className="border-t border-gray-200 dark:border-gray-800">
        <div className="border-b border-gray-200 p-4 lg:p-10 dark:border-gray-800">
          <h2 className="max-w-[20ch] text-4xl font-semibold tracking-tight text-balance text-gray-900 lg:text-5xl dark:text-white">
            Follow your models as you type
          </h2>
          <p className="mt-4 max-w-[64ch] text-base text-pretty text-gray-600 dark:text-gray-300">
            This editor view is generated from the real language server and the checked-in Django
            fixture, so the documentation changes whenever completion behavior changes.
          </p>
        </div>
        <AutocompleteDemo example="field-lookups" />
      </section>
      <HomeCTA />
      <SiteFooter githubUrl={props.githubUrl} navLinks={navLinks} />
    </HomePage>
  );
}
