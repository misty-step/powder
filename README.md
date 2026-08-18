# Powder

Self-hosted exclusive-work ledger. Take a known job.

```sh
go build -o powder .
./powder serve --bind 127.0.0.1:4175
./powder create --id first --title "It exists" --spec "The card can be taken."
./powder list --takeable
./powder take first --agent me
./powder done first --proof https://example.test/proof
```

`powder <verb> --help` is flag truth. JSON on stdout. `--plain` for text
list/show. Errors are JSON on stderr with a `code`.

The Rust service that previously lived in this repository is retired.
Git history is intact. The live process is `powder serve`.
