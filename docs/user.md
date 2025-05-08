# How to Use `bls-runtime`

## Install binary

1. Clone the repository and build the runtime using Cargo

```bash
git clone https://github.com/blocklessnetwork/bls-runtime.git
cd bls-runtime
cargo build --release
```
This will generate the bls-runtime binary in the target/release/ directory.

2. Download binary release from the repository

```bash
curl -L https://github.com/blessnetwork/bls-runtime/releases/download/v0.7.0/blockless-runtime.macos-latest.aarch64.tar.gz| tar -xz
```
**Note: Please select the appropriate version based on your operating system and CPU architecture.**

Try to execute binary

```bash
./bls-runtime --help
```

![](images/help.jpg "Command Help")


