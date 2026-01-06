# saas-payment

`saas-payment` is a command-line tool written in Rust to pay the Stake-as-a-Service invoices from [The Vault](https://thevault.finance) stake pool.

It is derived from the Typescript [SaaS payment bot](https://github.com/SolanaVault/saas-payment-bot).

## Installation
1. Clone the repository:
```sh
git clone https://github.com/puffin-validator/saas-payment.git
````

2. Change to the projet directory:
```sh
cd saas-payment
````

3. Compile the tool
```sh
cargo build --release
```

The resulting binary is located in `target/release/`.

## Usage
The vote account address must be provided. Without any further argument, the outstanding invoiced will be listed and
the tool will exit.
To pay the invoice, also provide the path to the payer keypair file.

```console
$ saas-payment -v PUFFiNkUHF2DMfbKeUcYTSQckDDtkswfxZCDv5WQqwp -p ~/.config/solana/id.json
Epoch 901: 0.055544862 VSOL
VSOL balance: 0
Will deposit 0.062678587 SOL to get 0.055544862 VSOL
Proceed to payment?
y
obg8wjgdVrTTPb7Gtv2HyC2hnKBqTXPhDYSVXuMbUgTf5BZ7yv4WP6wmCVUqPVxcR2BTvk2UrueXsT3tvizThdy
```

Other optional arguments are `--auto` to pay without prompting for a confirmation and `--rpc` to specify the RPC
server.

For a full list of available options, use the `--help` flag.