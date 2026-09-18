# Releasing

The [Release workflow](../.github/workflows/release.yml) runs when a `v*` tag is
pushed. It checks that the tag is a stable version matching `Cargo.toml`, then
lints, tests, and builds all four supported platform targets. After every build
succeeds, separate jobs publish the crate to crates.io and the binary archives
to GitHub Releases.

## One-time crates.io setup

The crate already exists on crates.io. As a crate owner, open
[its settings](https://crates.io/crates/agy-auto-approve/settings), find
**Trusted Publishing**, and add a **GitHub** publisher with these exact values:

| Field | Value |
| --- | --- |
| Repository owner | `jjyr` |
| Repository name | `agy-auto-approve` |
| Workflow filename | `release.yml` |
| Environment | Leave empty (the publishing job does not use an environment) |

Use the workflow filename, not `.github/workflows/release.yml`. Save this binding
before pushing the first release tag that includes the workflow changes.

The workflow uses `rust-lang/crates-io-auth-action@v1` to exchange GitHub's OIDC
identity for a short-lived crates.io token. Only `publish-crate` has
`id-token: write`; its token is passed only to `cargo publish --locked`.
Package verification runs before authentication, and the action revokes the
token when the job finishes. No `CARGO_REGISTRY_TOKEN` repository secret is needed.
After a successful trusted publish, any old publishing token secret can be
removed if no other workflow needs it.

See the [official trusted publishing documentation](https://crates.io/docs/trusted-publishing).

## Publish a version

1. Bump `Cargo.toml` and `Cargo.lock` using `python3 scripts/bump-version.py`
   (patch by default), review the version commit, and merge it into `main`.
2. Tag the merged commit with the matching stable version, for example
   `git tag v0.4.5 <merged-commit>`, then push it with `git push origin v0.4.5`.
   The tagged commit must include the trusted publishing workflow.
3. Check both the `publish-crate` and `publish` jobs in GitHub Actions.

The crate job runs `cargo publish --locked --dry-run` before obtaining a token,
then publishes the verified package. GitHub Release and crates.io publishing
are independent after the build gate: if one fails, use **Re-run failed jobs**
after fixing the external configuration. Avoid rerunning successful publishing
jobs: crates.io versions cannot be overwritten, and the GitHub Release job
refuses to overwrite a published release. Code changes require a new commit
and a new release version rather than moving an existing release tag.
