# Innsbruck Bus

Live departures, a trip planner and the line network for Innsbruck's IVB
buses and trams, on Android. Built with [dfui](https://github.com/LuisAyuso/dfui).

- **Haltestellen**: search a stop and see its next departures; plan a trip
  from there with up to two changes.
- **Liniennetz**: the network as a graph. Tap a stop and the others arrange
  around it by travel time; the line chips turn lines on and off.
- **Linien**: every line with its stops and the lines to change to.
- **Karte**: a map of central Innsbruck with street names; drag to pan,
  pinch to zoom.

## Install

Download `busapp.apk` from the
[latest release](https://github.com/LuisAyuso/busapp/releases/latest) on the
phone and open it.

## Development

Expects a [dfui](https://github.com/LuisAyuso/dfui) checkout next to this
one (`../dfui`).

```sh
cargo run                    # desktop
cargo test
cargo clippy --all-targets
cargo apk build --lib --target x86_64-linux-android   # emulator
```

Build x86_64 for the emulator: the arm64 build runs there through
translation, and typing in a text field crashes it.

The line network in `assets/network.json` is a snapshot crawled from IVB's
API; refresh it with `cargo test --lib refresh_network -- --ignored`
(about a minute).

The map in `assets/innsbruck.json` is OpenStreetMap data (© OpenStreetMap
contributors, ODbL) for 4×4 km around the centre, drawn with dfui's
experimental `dfui-map`; refresh it with
`cargo test --lib refresh_map -- --ignored`.

## Releases

Pushing a `v*` tag builds a signed APK and attaches it to a GitHub release:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

Running the workflow by hand (Actions → release → Run workflow) publishes a
`build-<sha>` release instead. CI checks out dfui's `main` next to this repo.

The APK is signed with `~/.android/busapp-release.jks` (password in
`~/.android/busapp-release.password`), stored in the
`ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD` secrets. Keep a
backup of both: updates only install over an APK signed with the same key.
