# Amp orb operations

Use `./script/amp` for project-owned orb commands and read
`docs/src/development/amp-orbs.md` before adding credentials or operating
deployment infrastructure.

The normal sequence inside an orb is:

```sh
./script/amp bootstrap
./script/amp doctor
amp orb service ensure --json
./script/amp screenshot
```

The screenshot is written under the gitignored `.amp/in/artifacts/` directory.
No secret is required for this sequence.
