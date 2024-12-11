# Cardamon

<img src="https://github.com/nenioscio/cardamon/blob/7e88ef4396dbbeb806749bfd275c73ffd13fb374/cardmon.png?raw=true" width="350" title="cardamon logo">

## Overview

**Cardamon** is a CRD (Custom Resource Definition) monitor implemented in Rust. It is designed to help developers and system administrators monitor and manage Kubernetes CRDs efficiently. In case a CRD cannot be read correctly, this may suggest either missing permissions or for example broken and misconfigured webhooks.

## Features

- **Efficient Monitoring**: Continuously monitors CRDs for changes and updates.
- **High Performance**: Built with Rust for speed and safety.
- **Easy Integration**: Seamlessly integrates with existing Kubernetes clusters.

## Installation

To install Cardamon, you need to have Rust and Cargo installed. You can install Rust using rustup.

```sh
# Clone the repository
git clone https://github.com/nenioscio/cardamon.git

# Navigate to the project directory
cd cardamon

# Build the project
cargo build --release
```

## Usage

After building the project, you can run the CRD monitor with the following command:

```sh
./target/release/cardamon
```

## Configuration

Cardamon can be configured using a configuration file. The default configuration file is `config.yaml`. You can customize it according to your needs.

```yaml
# Example configuration
apiVersion: v1
kind: Config
metadata:
  name: cardamon-config
spec:
  monitorInterval: 30s
  logLevel: info
```

## Contributing

Contributions are welcome! Please feel free to submit a pull request or open an issue.

## License

This project is licensed under the MIT License. See the LICENSE file for details.

## Contact

For any questions or suggestions, please open an issue or contact the repository owner.
