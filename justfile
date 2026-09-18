set dotenv-load

alias b := build
alias c := check
alias d := doc
alias r := run
alias t := test

[private]
default:
    @just --list

check:
    prek run --all-files

audit:
    cargo audit

build *args:
    cargo build --release {{ args }}

doc *args:
    cargo doc {{ args }}

run *args:
    cargo run {{ args }}

test *args:
    cargo test {{ args }}

ci: audit check build doc test
