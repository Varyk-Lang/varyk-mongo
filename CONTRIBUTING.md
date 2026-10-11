# Contributing

varyk-mongo is the official MongoDB package for Varyk. Varyk is
experimental and pre-1.0: anything may change, and a new feature or a
breaking change bumps the minor version. Contributions are welcome;
small, focused pull requests are the easiest to review. The project has
one maintainer, so reviews are best effort and a pull request may wait
a while; a reminder after two weeks is welcome.

## Build and test

You need Rust 1.88 or later through [rustup](https://rustup.rs), Docker
for the test server, and `varyk` (`cargo install varyk --version
'^0.8.1' --locked`, the latest 0.8 release, as CI uses). The package is
a Varyk package (`src/lib.vr`), so it is built only by `varyk`; plain
`cargo build` does not work here.

The tests run against a real server, since MongoDB has no in-memory
mode, and the transaction tests need a replica set. Start a replica set
of one server once:

```sh
docker run -d --name varyk-mongo-test -p 27017:27017 mongo:8.0 --replSet rs0 --bind_ip_all
docker exec varyk-mongo-test mongosh --quiet --eval "rs.initiate({ _id: 'rs0', members: [{ _id: 0, host: '127.0.0.1:27017' }] })"
docker exec varyk-mongo-test mongosh --quiet --eval "db.hello().isWritablePrimary"
```

The last line prints `true` once the server is ready, after a few
seconds. The tests use `MONGO_URL`, or
`mongodb://127.0.0.1:27017/varyk_test` when it is not set. When port
27017 is taken, run the server on another port, name the member after
it, and set `MONGO_URL` to match:

```sh
docker run -d --name varyk-mongo-test -p 27018:27018 mongo:8.0 --replSet rs0 --bind_ip_all --port 27018
docker exec varyk-mongo-test mongosh --port 27018 --quiet --eval "rs.initiate({ _id: 'rs0', members: [{ _id: 0, host: '127.0.0.1:27018' }] })"
export MONGO_URL=mongodb://127.0.0.1:27018/varyk_test
```

Before opening a pull request, run the same checks CI runs:

```sh
varyk check
varyk test
rustfmt --edition 2024 --check src/*.rs
(cd demo/users && varyk run)
(cd "$(varyk publish --assemble-only)" && cargo clippy --all-targets -- -D warnings)
```

The demo prints `true 1 1`. `varyk publish --assemble-only` writes the
plain Rust crate the package publishes as, and prints its folder; clippy
runs there. `.github/workflows/ci.yml` has the exact steps.

Each test works in a collection named after itself and deletes its
documents first, so the tests run in parallel and a rerun starts clean.
Keep it so for a new test.

The minimum supported Rust version is 1.88, which the `mongodb` crate
needs, and CI checks it. Every Monday, and on demand from the Actions
tab, the "Latest varyk" workflow builds and tests varyk-mongo with the
newest varyk on crates.io, moving `varyk-std` to its version for that
run. It is not a required check; a red run is an early warning that a
newer varyk changed something varyk-mongo's Rust calls. A release that
moves `varyk-std` follows each minor varyk when convenient, not at
once.

## Where things are

- `src/lib.vr` is everything a program sees. `src/db.rs` is the facade
  over the `mongodb` crate and the only file Varyk imports items from;
  `src/text.rs` parses the query text, `src/write.rs` writes a document,
  and `src/read.rs` reads one. `db.rs` reaches the other three as
  `crate::text`, `crate::write`, and `crate::read`.
- None of them panics: no `unwrap`, `expect`, `unreachable!`, or
  indexing or slicing that can fail, and every failure becomes a
  `varyk_std::Error` whose message names no URL, no password, and no
  value.
- `src/tests.vr` holds the Varyk tests, as a module: a Varyk package may
  not have a `tests/`, `examples/`, or `benches/` directory. What needs
  no server (the parser, the writer, the reader) is tested in each
  `.rs` file's own unit tests.
- `demo/users/` is the program of the README.
- `docs/specs/` holds the design; read it before changing what the
  package offers.

Rust code follows Varyk's
[AGENTS.md](https://github.com/Varyk-Lang/varyk/blob/main/AGENTS.md):
the smallest change that works, failures with plain-word messages, and
safe by default. Here that means query texts stay literal, with no way
to build one from input, and a value stands only where the allowlist of
the README's "The query text" lets MongoDB read it as plain data.

## Commit messages and releases

Releases are cut by
[release-please](https://github.com/googleapis/release-please) from the
commit history on `main`, so every commit that lands on `main` carries a
conventional prefix. A pull request with one concern is squash-merged
with a conventional title; a pull request with several concerns is
rebase-merged with one conventional commit per concern.

| Prefix | Use it for | Effect on the release |
|---|---|---|
| `feat:` | a new capability | bumps the version, listed in the changelog |
| `fix:` | a bug fix | bumps the version, listed in the changelog |
| `docs:` | documentation only | no bump, not listed |
| `test:` | tests only | no bump, not listed |
| `ci:` | workflows and release automation | no bump, not listed |
| `chore:` | maintenance, dependencies, manifests | no bump, not listed |
| `refactor:` | code change with no behavior change | no bump, not listed |

A `!` after the prefix (`feat!:`) marks a breaking change. Before 1.0,
`feat` and a breaking change bump the minor version and `fix` bumps the
patch version.

Keep `<` and `>` out of commit subjects and `BREAKING CHANGE:` footers:
write `Option of T`, not `Option<T>`. release-please copies them into
the release pull request and reads that body as HTML, where an unclosed
`<T>` hides what follows it. The `Commit messages` check enforces this
on every pull request.

## Licensing of contributions

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in varyk-mongo by you, as defined in the
Apache-2.0 license, shall be dual licensed under MIT or Apache-2.0,
without any additional terms or conditions. The name "Varyk" is a
trademark and is not covered by those licenses; see Varyk's
[TRADEMARKS.md](https://github.com/Varyk-Lang/varyk/blob/main/TRADEMARKS.md).

## Reporting bugs

Open an issue with the smallest Varyk program that shows the problem,
the MongoDB version and whether it is a replica set, and the error
message. For security problems, see the security policy shared by every
Varyk-Lang repository:
<https://github.com/Varyk-Lang/varyk-mongo/security/policy>.
