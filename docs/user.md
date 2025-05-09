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
sh -c "curl https://raw.githubusercontent.com/blessnetwork/bls-runtime/refs/heads/main/install.sh | bash"
```
**Note: Please select the appropriate version based on your operating system and CPU architecture.**

Try to execute binary

```bash
./bls-runtime --help
```

![](images/help.jpg "Command Help")

## How to build the wasm

### Download the bls-javy

```bash
sh -c "curl https://raw.githubusercontent.com/blocklessnetwork/bls-javy/main/download.sh | bash"
```

### Write your hello world

Save code to the hello.js file
```javascript
function helloWorld() {
    console.log("hello World")
}
helloWorld()
```

Build the code to wasm

```bash
bls-javy build hello.js -o hello.wasm
```

Run the wasm with runtime

```bash
bls-runtime  hello.wasm
```




