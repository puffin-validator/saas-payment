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

Version 1.0.0 will only pay invoices denominated in VLP. If you still have outstanding invoices denominated in VSOL, 
use previous version first (0.1.1) to pay them.

```console
$ saas-payment -v PUFFiNkUHF2DMfbKeUcYTSQckDDtkswfxZCDv5WQqwp -p ~/.config/solana/id.json
Epoch 1065 (DzWNQFv5FPVwpiSehxDJJ8KMMMp89XEjVswsAmqB5ARF): 0.053008595 VLP
Epoch 1066 (DzWNQFv5FPVwpiSehxDJJ8KMMMp89XEjVswsAmqB5ARF): 0.050416944 VLP
VLP balance: 0
Current VLP rate is 1.004 SOL/VLP
Will deposit 0.109031203 SOL to get at least 0.103425539 VLP (5% margin)
Proceed to payment?
y
5ZwtuG3RRuVbQ9RzyJ9PzmoZKXdvTgFLN8y6zfkCkrThvkHP75VfSHrinc1bHtNmrWaGpeG6zyZ5d33QCm5NoUSB
```

Other optional arguments are `--auto` to pay without prompting for a confirmation and `--rpc` to specify the RPC
server.

For a full list of available options, use the `--help` flag.