$Env:RUSTFLAGS="-Clink-arg=-Wl,-rpath,`$ORIGIN"
cargo build -r
