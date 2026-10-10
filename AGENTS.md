# Working on varyk-mongo

This file is for anyone, human or AI agent, making changes to this
repository. [CONTRIBUTING.md](CONTRIBUTING.md) has the details; this is
the short list of rules.

## What this is

varyk-mongo is the official MongoDB package for Varyk, over the official
`mongodb` crate. `src/lib.vr` is everything a program sees, `src/db.rs`
is the facade, `src/text.rs` parses the query text, `src/write.rs` and
`src/read.rs` write and read documents, and `src/tests.vr` holds the
tests. The design is in `docs/specs/`; read it before changing what the
package offers.

## The gate

Run the checks CONTRIBUTING.md lists ("Build and test") before every
pull request: `varyk check`, `varyk test` against a replica set of one
server, rustfmt on `src/*.rs`, the demo, and clippy on the crate
`varyk publish --assemble-only` writes. CI's `test` and `msrv` jobs are
required checks: keep those job names, and keep the code building on
Rust 1.88.

## Rules

- **No crash on absence.** No `unwrap`, `expect`, `unreachable!`, or
  indexing or slicing that can fail in `src/*.rs`. Every failure is a
  `varyk_std::Error`.
- **No secret in an error.** No message the package writes holds the
  URL, a password, a value beside a text, or a value read from a
  document; a message names a place in a text, a value's number, or a
  field. Only the server's and the driver's own message text passes
  through.
- **Query texts stay literal.** Every text, and a collection's name, is
  a literal; never add a way to build one from input.
- **Values only where the allowlist says.** A `?` is taken only where
  MongoDB reads plain data (a filter, a top-level `$match`, a value of
  `$set` and the like update operators, a `$pull` condition, a
  top-level `$skip` or `$limit`); open a new place only with a change to
  the spec.
- **Commits.** Conventional prefixes (`feat:`, `fix:`, `docs:`, ...),
  no `<` or `>` in subjects or `BREAKING CHANGE:` footers, and no
  attribution lines (no `Co-Authored-By`, no "Generated with").
- **Docs move with the code.** README.md, CONTRIBUTING.md,
  `docs/specs/`, and the demo are updated in the same change whenever
  what they describe changes: status, versions, install, usage,
  behavior, links. Check them against the change before every pull
  request.
