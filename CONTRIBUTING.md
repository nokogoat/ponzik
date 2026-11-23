# Contributing to Ponzik

Thanks for helping! Ponzik is free for everyone, and every bug report, idea or line
of code makes it better.

## Report a bug or ask for something

[Open an issue](https://github.com/nokogoat/ponzik/issues/new/choose). For a bug, say
what you did, what you expected and what happened, with your system (Linux or
Windows) and the Ponzik version (Settings > About). A screenshot or the project file
helps a lot.

## Change the code

1. **Fork** the repository (button at the top right of GitHub): you get your own copy.
2. **Clone** your fork and create a branch: `git checkout -b my-change`.
3. **Build and run**: see [docs/BUILDING.md](docs/BUILDING.md). Read [AGENTS.md](AGENTS.md)
   first: it lists the rules (no external crates, the audio thread, translations, tests).
4. **Commit** small, focused changes with English messages: `Mixer: bus sends`.
5. **Check** before pushing:
   ```sh
   cargo build -p ponzik-clap-test-plugin -p ponzik-vst3-test-plugin
   cargo clippy --all-targets
   cargo test --workspace
   ```
6. **Push** your branch and open a **pull request** to `main`. Describe what changes for
   the user, with a screenshot for anything visible.

A maintainer reviews every pull request. Nothing reaches `main`, the nightly build or
the automatic updates without that review.

## Using an AI assistant

[AGENTS.md](AGENTS.md) (and CLAUDE.md, which points to it) gives coding assistants
everything they need: the layout, the rules and how to verify a change with
screenshots. You stay responsible for what you submit: read it, test it.

## Translations

Ponzik's interface can be translated without writing code: see
[docs/TRANSLATING.md](docs/TRANSLATING.md).

## License

By contributing you agree that your work is published under the
[GPL-3.0-or-later](LICENSE), like the rest of Ponzik.
