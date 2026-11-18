# Changelog

What changed in each version of TessaFab: the app, the engine and the
`tessafab` command line, which are released together. The installers are on
the [releases page](https://github.com/tessafab/tessafab/releases).

Write changes under **Unreleased** as they are made, for the people who use
TessaFab: what they'll notice, not how it was done. `tools/release/release.sh` turns
that section into the version's entry when a release is made.

## Unreleased

### New

- **Sign in with your email.** The first time you open TessaFab, enter your email and we'll send you a 6-digit code. There's no password, and signing in makes your free account. After that TessaFab works offline, as before.
- **Your account is in Account and licence** (the Licence sheet's new name). Sign out there, or delete your account. Deleting it signs you out on every computer; a Pro licence keeps working with its key.

## 0.2.1 (2026-11-18)

### New

- **TessaFab tells you when a newer version is out.** A note in the top bar links to the download. If your Pro licence's updates ended before that version, the note says so and links to renewing them. Dismiss it to hide it until the next version.
- **Terms, privacy and refunds** are on tessafab.com and linked from the Licence sheet. TessaFab Pro comes with a 30-day money-back guarantee.

## 0.2.0 (2026-11-17)

### New

- **TessaFab Pro is for sale.** **Buy TessaFab Pro** in the Licence sheet opens the checkout. Your licence key is shown as soon as you've paid, and emailed to you. Paste it into the Licence sheet to activate.

### Changed

- **The free edition shows every piece.** Assemble and Pattern now show the whole assembly instead of a single piece. The free edition still patterns one piece; the others are greyed out, and clicking one patterns it instead. Projects keep every piece, so they open whole in Pro.
- **Pro features show in the free edition, greyed out.** **Depth**, **In a row** and **In a grid**, **Pattern each piece alike** and patterns from an image are shown where they belong, locked, with a note on hover saying they come with TessaFab Pro.

### Fixed

- **Naming a new preset:** the **Save** and **Cancel** buttons beside the name no longer run off the edge of the side panel.

## 0.2.0-beta.3 (2026-11-17)

### Changed

- **One TessaFab logo at every size.** The app icon and the logo now have two lines in each tile everywhere, as the small icons already did, so they look the same in the Dock, the menu bar and on the website. The jigsaw example's wordmark pattern uses it too.

### Fixed

- **The Mac app opens after it's downloaded.** It's now signed with a Developer ID and notarized by Apple, so macOS no longer says it can't be opened, and it needs no workaround.

## 0.2.0-beta.1 (2026-11-16)

### New

- **A patterned bottom takes the pattern depth.** With **Pattern depth** at 2 or more and the bottom patterned, the bottom's solid layers above the first get the same tiles too, without grout (the bottom lies on the plate). It stops where the slicer's bottom shell layers do, and says so.
- **The hexagon tiles example** is now seven hexagons on one plate, spaced apart, with the pattern carrying on across the gaps. It's also an STL, `examples/hexagons.stl`, to slice yourself.

### Changed

- **Relief is now called Depth**, and grout depth is **Pattern depth**: the number of layers the pattern goes into, counting the surface. Project files keep `relief.groutDepth`, so saved projects open as before.

### Fixed

- **Preview counted the start of the print as a layer.** The purge line before the first layer showed as layer 1, so the real first layer looked like layer 2. It's now drawn with the first layer, and each layer shows its own height.
- **Export could stop with "The printer would be left in a different state"** on slices whose speeds have fractions (`F9466.211`, common with concentric surfaces): the speed restored after a patterned surface was rounded. It's now restored exactly.
