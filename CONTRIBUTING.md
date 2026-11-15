# Contributing

Thank you for helping. Bug reports, pattern ideas and pull requests are
welcome.

## Before you open a pull request

- **Contributor licence agreement.** The first time you open a pull request,
  you'll be asked to sign the contributor licence agreement (CLA). It lets
  your contribution ship in the official TessaFab app as well as here; you
  keep the copyright in your work.
- Run the checks CI runs:

  ```sh
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  ```

- Keep the engine free of anything specific to one model or one printer:
  features work on any sliced plate.
- A change that alters the G-code the engine writes should say so, and why,
  in the pull request.

## Where things are

See [docs/engine.md](docs/engine.md) for the crates, how a session works and
the geometry notes behind some of the engine's choices.

## Licence

By contributing you agree that your contribution is licensed under this
repository's licences (PolyForm Noncommercial 1.0.0 for code, CC BY-NC 4.0 for
patterns and examples) and under the CLA.
