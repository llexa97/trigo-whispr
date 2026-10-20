# Release Notes

Add user-facing release notes as Markdown files named by app version:

```text
src/content/release-notes/0.8.4.md
```

The What's New modal no longer opens after an update; it can only be previewed
from the Debug settings page, which shows the latest bundled release note.

Keep these files focused on headline user-facing changes. Release notes support
paragraphs, headings, lists, links, code, quotes, separators, hard line breaks,
and local images under `/release-notes/...`. Raw HTML is ignored before
rendering.

Place image assets in `public/release-notes/{version}/` and reference them from
Markdown with absolute paths:

```md
![Streaming transcription preview](/release-notes/0.9.0/streaming-transcription.webp)
```
