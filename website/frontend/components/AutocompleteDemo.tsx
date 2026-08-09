import type { CSSProperties } from "react";

import examples from "../generated/completions.json";

interface AutocompleteDemoProps {
  compact?: boolean | string;
  example: string;
}

interface Token {
  kind: "comment" | "keyword" | "member" | "name" | "plain" | "string";
  value: string;
}

interface CompletionItem {
  kind: "field" | "lookup";
  label: string;
  matched: string;
  rest: string;
}

const lookupNames = new Set([
  "exact",
  "iexact",
  "contains",
  "icontains",
  "in",
  "gt",
  "gte",
  "lt",
  "lte",
  "startswith",
  "istartswith",
  "endswith",
  "iendswith",
  "range",
  "isnull",
  "regex",
  "iregex",
  "date",
  "year",
  "month",
  "day",
  "week",
  "week_day",
  "quarter",
  "time",
  "hour",
  "minute",
  "second",
  "search",
]);

const tokenPattern =
  /(#.*$|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\b(?:as|class|def|from|import|return)\b|\b[A-Z][A-Za-z0-9_]*\b|\.[A-Za-z_][A-Za-z0-9_]*)/g;

function tokenize(value: string): Token[] {
  const tokens: Token[] = [];
  let offset = 0;

  for (const match of value.matchAll(tokenPattern)) {
    const index = match.index ?? 0;
    if (index > offset) {
      tokens.push({ kind: "plain", value: value.slice(offset, index) });
    }

    const token = match[0];
    const kind = token.startsWith("#")
      ? "comment"
      : token.startsWith('"') || token.startsWith("'")
        ? "string"
        : token.startsWith(".")
          ? "member"
          : /^(?:as|class|def|from|import|return)$/.test(token)
            ? "keyword"
            : "name";
    tokens.push({ kind, value: token });
    offset = index + token.length;
  }

  if (offset < value.length) {
    tokens.push({ kind: "plain", value: value.slice(offset) });
  }

  return tokens;
}

function TokenizedLine({ value }: { value: string }) {
  return tokenize(value).map((token, index) => (
    <span key={`${index}-${token.value}`} className={`token-${token.kind}`}>
      {token.value}
    </span>
  ));
}

function CompletionIcon({ kind }: { kind: CompletionItem["kind"] }) {
  return (
    <span className={`completion-icon kind-${kind}`} aria-hidden="true">
      {kind === "lookup" ? (
        <svg viewBox="0 0 16 16" fill="currentColor">
          <path d="M2.6 2.8h10.8c.5 0 .8.6.5 1L9.8 8.7v4.4c0 .25-.15.48-.38.57l-2.4 1c-.4.17-.82-.13-.82-.57V8.7L2.1 3.8c-.3-.4 0-1 .5-1Z" />
        </svg>
      ) : (
        <svg
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.3"
          strokeLinejoin="round"
        >
          <path d="M8 1.8 13.8 5.1v5.8L8 14.2 2.2 10.9V5.1L8 1.8Z" />
          <path d="M2.5 5.3 8 8.4l5.5-3.1M8 8.4v5.6" />
        </svg>
      )}
    </span>
  );
}

function CompletionMenu({
  items,
  remainingItems,
  totalItems,
}: {
  items: CompletionItem[];
  remainingItems: number;
  totalItems: number;
}) {
  return (
    <div
      className="completion-menu completion-menu-quiet"
      aria-label={`${totalItems} completion suggestions`}
    >
      <ul role="list">
        {items.map((item, index) => (
          <li key={item.label} className={index === 0 ? "selected" : undefined}>
            <CompletionIcon kind={item.kind} />
            <span className="completion-label">
              {item.matched && <span className="completion-match">{item.matched}</span>}
              {item.rest}
            </span>
          </li>
        ))}
      </ul>
      {remainingItems > 0 && (
        <p className="completion-more">
          {remainingItems} more {remainingItems === 1 ? "result" : "results"}
        </p>
      )}
    </div>
  );
}

export function AutocompleteDemo({ compact = false, example: exampleId }: AutocompleteDemoProps) {
  const example = examples.find((candidate) => candidate.id === exampleId);
  if (!example) {
    throw new Error(`Unknown django-lsp example: ${exampleId}`);
  }

  const isCompact = compact === true || compact === "" || compact === "true";
  const lines = example.source.split("\n");
  const cursorLine = lines[example.cursor.line] ?? "";
  const typedPrefix =
    cursorLine
      .slice(0, example.cursor.character)
      .match(/[A-Za-z_][A-Za-z0-9_]*$/)?.[0] ?? "";
  const itemLimit = isCompact ? Math.min(example.visibleItems, 5) : example.visibleItems;
  const visibleItems: CompletionItem[] = example.items.slice(0, itemLimit).map((label) => {
    const matched = typedPrefix && label.startsWith(typedPrefix) ? typedPrefix : "";
    const separator = label.lastIndexOf("__");
    const suffix = separator === -1 ? label : label.slice(separator + 2);
    return {
      kind: lookupNames.has(suffix) ? "lookup" : "field",
      label,
      matched,
      rest: label.slice(matched.length),
    };
  });
  const remainingItems = example.items.length - visibleItems.length;

  const fixtureSplit = example.fixture.lastIndexOf("/") + 1;
  const fixtureDir = example.fixture.slice(0, fixtureSplit);
  const fixtureName = example.fixture.slice(fixtureSplit);
  const modelsSplit = example.modelsFixture.lastIndexOf("/") + 1;
  const modelsDir = example.modelsFixture.slice(0, modelsSplit);
  const modelsName = example.modelsFixture.slice(modelsSplit);

  const modelsPane = isCompact
    ? null
    : (() => {
      const anchor = example.source.match(/from \.models import (\w+)/)?.[1];
      const allLines = example.modelsSource.split("\n");
      const modelCount = allLines.filter((line) => line.startsWith("class ")).length;
      let start = anchor
        ? allLines.findIndex((line) => line.startsWith(`class ${anchor}(`))
        : -1;
      if (start === -1) start = 0;
      const nextModel = allLines
        .slice(start + 1)
        .findIndex((line) => line.startsWith("class "));
      const end = nextModel === -1 ? allLines.length : start + nextModel + 1;
      const lines = allLines.slice(start, end);

      while (lines.at(-1)?.trim() === "") lines.pop();

      return { lines, otherModels: Math.max(0, modelCount - 1) };
    })();

  const editorVariables = {
    "--completion-items": visibleItems.length,
    "--completion-more": remainingItems > 0 ? 1 : 0,
    "--cursor-column": example.cursor.character,
    "--cursor-line": example.cursor.line,
  } as CSSProperties;

  return (
    <figure
      className={`autocomplete-demo${isCompact ? " compact" : ""}`}
      aria-label={`Autocomplete result for ${example.id}`}
    >
      <div className={`editor-split${isCompact ? "" : " split"}`}>
        <div className="editor-shell">
          <div className="editor-tabs" aria-hidden="true">
            <span className="editor-tab">
              {fixtureDir && <span className="tab-dir">{fixtureDir}</span>}
              {fixtureName}
            </span>
          </div>
          <div className="editor-body" style={editorVariables}>
            <div className="code-lines" aria-label={`Python source from ${example.fixture}`}>
              {lines.map((line, index) => {
                const hasCursor = index === example.cursor.line;
                const beforeCursor = hasCursor
                  ? line.slice(0, example.cursor.character)
                  : line;
                const afterCursor = hasCursor ? line.slice(example.cursor.character) : "";

                return (
                  <div
                    key={`${index}-${line}`}
                    className={`code-line${hasCursor ? " has-cursor" : ""}`}
                  >
                    <span className="line-number" aria-hidden="true">
                      {index + 1}
                    </span>
                    <code>
                      <TokenizedLine value={beforeCursor} />
                      {hasCursor && <span className="editor-caret" aria-hidden="true" />}
                      <TokenizedLine value={afterCursor} />
                    </code>
                  </div>
                );
              })}
            </div>

            <CompletionMenu
              items={visibleItems}
              remainingItems={remainingItems}
              totalItems={example.items.length}
            />
          </div>
        </div>

        {modelsPane && (
          <aside className="models-pane" aria-label={`Django models from ${example.modelsFixture}`}>
            <p className="pane-label" aria-hidden="true">
              {modelsDir && <span className="tab-dir">{modelsDir}</span>}
              {modelsName}
            </p>
            <div className="models-lines">
              {modelsPane.otherModels > 0 && (
                <div className="models-line muted">
                  … {modelsPane.otherModels} other{" "}
                  {modelsPane.otherModels === 1 ? "model" : "models"}
                </div>
              )}
              {modelsPane.lines.map((line, index) => (
                <div key={`${index}-${line}`} className="models-line">
                  <TokenizedLine value={line} />
                </div>
              ))}
            </div>
          </aside>
        )}
      </div>
    </figure>
  );
}
