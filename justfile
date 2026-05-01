default: check run
alias r := run
run:
    cargo run
alias c := check
check:
    prek run --all-files
