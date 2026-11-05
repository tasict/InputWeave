# Release notes

One file per version, `<version>.md`, in English. `scripts/release.sh` puts it in three places:

- the GitHub release's description, followed by every file's SHA-256;
- Sparkle's update window on macOS, as HTML (`appcast.xml`);
- the Windows app's update dialog, as plain text (`update-windows.json`).

So keep to the Markdown those can carry:

- the first line is the title, `# InputWeave <version>`, and is left out of the feeds;
- `## ` headings;
- paragraphs separated by blank lines;
- items starting with `- ` (continuation lines indented by two spaces);
- `` `code` ``, `**bold**` and `[text](https://…)` links inside a line. Link to the website's stable addresses (`https://inputweave.ctsite.cc/user-guide`, `/releases/v<version>`, see `site/_redirects`), not to GitHub, so the notes keep working if the releases move.

Write for the people installing the update: what changed for them, in plain words, newest and most noticeable first. Fixes say what used to go wrong. Leave out internal changes that make no difference to them.

`release.sh` refuses a file that still contains `TODO` or `[TBD]`, except in a dry run.
