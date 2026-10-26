use borsh::BorshSerialize;
use clap::Parser;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::client_error::ErrorKind;
use solana_signer::Signer;
use solana_transaction::Transaction;
use spl_associated_token_account_client::address::get_associated_token_address;
use spl_associated_token_account_client::instruction::create_associated_token_account;
use solana_pubkey::{Pubkey, pubkey};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(version, about = "Pay The Vault Stake-as-a-service invoices")]
struct Args {
    #[arg(
        short,
        long,
        help = "URL of the RPC used to fetch invoices and send transactions",
        default_value = "https://api.mainnet-beta.solana.com"
    )]
    rpc: String,

    #[arg(short, long, help = "Path to payer keypair file")]
    payer: Option<PathBuf>,

    #[arg(short, long, help = "Voting pubkey")]
    vote_account: String,

    #[arg(short, long, help = "If set, invoices are payed without asking for confirmation", default_value_t = false)]
    auto: bool,
}

// Trusted invoicers
const INVOICERS: [Pubkey; 2] = [
    pubkey!("9HE9R14dazhR1rz6Vamap7zTzkVihqiWVk5nS447PB6F"), // community pool
    pubkey!("DzWNQFv5FPVwpiSehxDJJ8KMMMp89XEjVswsAmqB5ARF"), // SaaS
];
const INVOICER_PROGRAM: Pubkey = pubkey!("EpoivtVh9dgWFxE6MYgF3YnobYWtZr2VfCuP7iT3N927");
const LIQUID_UNSTAKER_PROGRAM: Pubkey = pubkey!("2rU1oCHtQ7WJUvy15tKtFvxdYNNSc3id7AzUcjeFSddo");
const LIQUID_UNSTAKER_POOL: Pubkey = pubkey!("9nyw5jxhzuSs88HxKJyDCsWBZMhxj2uNXsFcyHF5KBAb");
const VLP_MINT: Pubkey = pubkey!("EUWoTx5vQQrxaDFdeK2PLUVbnmRWjw4x6sBbmcxBaHjF");
const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const SYSTEM_PROGRAM: Pubkey = pubkey!("11111111111111111111111111111111");
const CLOCK_SYSVAR: Pubkey = pubkey!("SysvarC1ock11111111111111111111111111111111");

// Invoicer account (304 B): reserves ATA at @48, payment mint at @272
const INVOICER_RESERVES_OFFSET: usize = 48;
const INVOICER_MINT_OFFSET: usize = 272;

const SWAP_MARGIN: f64 = 1.05;

/// Returns the VLP reserves account of an invoicer, refusing any non-VLP invoicer.
fn get_invoicer_reserves(rpc_client: &RpcClient, invoicer: &Pubkey) -> Pubkey {
    let data = rpc_client
        .get_account_data(invoicer)
        .expect("Cannot get invoicer from RPC");
    assert_eq!(data.len(), 304, "Unexpected invoicer {} layout (legacy vSOL invoicer?)", invoicer);
    let mint = Pubkey::try_from(&data[INVOICER_MINT_OFFSET..INVOICER_MINT_OFFSET + 32]).unwrap();
    assert_eq!(mint, VLP_MINT, "Invoicer {} does not settle in VLP", invoicer);
    Pubkey::try_from(&data[INVOICER_RESERVES_OFFSET..INVOICER_RESERVES_OFFSET + 32]).unwrap()
}

fn find_invoice_address(invoicer: &Pubkey, vote_account: &Pubkey, epoch: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"invoice",
            invoicer.as_ref(),
            vote_account.as_ref(),
            &epoch.to_le_bytes(),
        ],
        &INVOICER_PROGRAM,
    )
    .0
}

struct Invoice {
    invoicer: Pubkey,
    invoice: Pubkey,
    epoch: u64,
    amount_vlp: u64,
    balance_outstanding: u64,
}

fn parse_invoice(pk: &Pubkey, data: &Vec<u8>) -> Invoice {
    let invoicer = Pubkey::try_from(&data[8..40]).unwrap();
    // 40..72 is vote address
    let epoch = u64::from_le_bytes((&data[72..80]).try_into().unwrap());
    let amount_vlp = u64::from_le_bytes((&data[80..88]).try_into().unwrap());
    let balance_outstanding = u64::from_le_bytes((&data[88..96]).try_into().unwrap());

    Invoice {
        invoicer,
        invoice: *pk,
        epoch,
        amount_vlp,
        balance_outstanding,
    }
}

fn get_invoices(rpc_client: &RpcClient, vote_account: &Pubkey) -> Vec<Invoice> {
    let epoch_info = rpc_client
        .get_epoch_info()
        .expect("Cannot get epoch info from RPC");
    let current_epoch = epoch_info.epoch;
    let addresses = INVOICERS
        .iter()
        .flat_map(|invoicer| {
            (current_epoch - 20..=current_epoch).map(|e| find_invoice_address(invoicer, vote_account, e))
        })
        .collect::<Vec<Pubkey>>();

    let infos = rpc_client
        .get_multiple_accounts(addresses.as_slice())
        .expect("Cannot get invoices from RPC");
    assert_eq!(infos.len(), addresses.len());
    let mut invoices = addresses
        .iter()
        .zip(infos.iter())
        .filter_map(|(pk, a)| {
            a.as_ref()
                .map(|a| parse_invoice(pk, &a.data))
                .filter(|i| i.balance_outstanding > 0 && i.amount_vlp > 0)
        })
        .collect::<Vec<Invoice>>();
    invoices.sort_by_key(|i| i.epoch);
    invoices
}

/// Price of one VLP in SOL, from the liquid unstaker pool:
/// (sol_vault_lamports + total_deactivating_stake + LST inventory value) / total_lp_tokens.
/// The LST inventory value is the snapshot kept in the pool's inventory_summary account.
fn get_vlp_price_sol(rpc_client: &RpcClient) -> f64 {
    let read_u64 = |data: &[u8], o: usize| u64::from_le_bytes(data[o..o + 8].try_into().unwrap());

    let data = rpc_client
        .get_account_data(&LIQUID_UNSTAKER_POOL)
        .expect("Cannot get liquid unstaker pool from RPC");
    assert_eq!(&data[..8], &[241, 154, 109, 4, 17, 177, 109, 188], "Invalid pool account");
    let total_lp_tokens = read_u64(&data, 136);
    let total_deactivating_stake = read_u64(&data, 152);
    let sol_vault_lamports = read_u64(&data, 179);
    assert!(total_lp_tokens > 0, "VLP supply is zero");

    let inventory_summary = Pubkey::find_program_address(
        &[b"inventory_summary", LIQUID_UNSTAKER_POOL.as_ref()],
        &LIQUID_UNSTAKER_PROGRAM,
    )
    .0;
    let data = rpc_client
        .get_account_data(&inventory_summary)
        .expect("Cannot get liquid unstaker inventory summary from RPC");
    assert_eq!(&data[..8], &[157, 77, 59, 213, 161, 246, 95, 57], "Invalid inventory summary account");
    let inventory_value = read_u64(&data, 40);

    (sol_vault_lamports + total_deactivating_stake + inventory_value) as f64 / total_lp_tokens as f64
}

#[derive(BorshSerialize)]
struct DepositSolParams {
    discriminator: [u8; 8],
    amount: u64,
}

fn swap_sol_for_vlp(
    ixs: &mut Vec<Instruction>,
    rpc_client: &RpcClient,
    payer: &Pubkey,
    vlp_ata: &Pubkey,
    vlp: u64,
) {
    let vlp_balance: u64 = match rpc_client.get_token_account_balance(&vlp_ata) {
        Err(e) => {
            match e.kind {
                ErrorKind::RpcError(_) => {
                    // Assuming not existent account
                    let ix = create_associated_token_account(&payer, &payer, &VLP_MINT, &TOKEN_PROGRAM);
                    ixs.push(ix);
                }
                _ => {
                    panic!("Cannot get VLP balance from RPC: {}", e);
                }
            }
            0
        }
        Ok(ui) => {
            println!("VLP balance: {} ", ui.ui_amount_string);
            ui.amount.parse::<u64>().unwrap()
        }
    };

    if vlp_balance < vlp {
        let price = get_vlp_price_sol(rpc_client);
        // Small margin for price drift since the inventory snapshot (extra VLP stays usable later)
        let sol = ((vlp - vlp_balance) as f64 * price * SWAP_MARGIN).ceil() as u64;

        let pda = |seed: &[u8]| {
            Pubkey::find_program_address(&[seed, LIQUID_UNSTAKER_POOL.as_ref()], &LIQUID_UNSTAKER_PROGRAM).0
        };

        let accounts = vec![
            AccountMeta::new(LIQUID_UNSTAKER_POOL, false),
            AccountMeta::new(pda(b"sol_vault"), false),
            AccountMeta::new(pda(b"lp_mint"), false),
            AccountMeta::new(*payer, true),
            AccountMeta::new(*vlp_ata, false),
            AccountMeta::new(pda(b"inventory_summary"), false),
            AccountMeta::new_readonly(CLOCK_SYSVAR, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ];

        ixs.push(Instruction::new_with_borsh(
            LIQUID_UNSTAKER_PROGRAM,
            &DepositSolParams {
                discriminator: [108, 81, 78, 117, 125, 155, 56, 200],
                amount: sol,
            },
            accounts,
        ));

        println!("Current VLP rate is {:.3} SOL/VLP", price);
        println!("Will deposit {} SOL to get at least {} VLP ({:.0}% margin)", sol as f64 / 1e9, (vlp - vlp_balance) as f64 / 1e9, (SWAP_MARGIN - 1.0) * 100.0);
    }
}

#[derive(BorshSerialize)]
struct PayInvoiceParams {
    discriminator: [u8; 8],
    amount_vsol: u64, // misnomer: amount in the invoicer's mint (VLP)
}

fn pay(ixs: &mut Vec<Instruction>, rpc_client: &RpcClient, invoices: &Vec<Invoice>, payer: &Pubkey, vlp_ata: &Pubkey) {
    for invoice in invoices {
        let vlp_reserve = get_invoicer_reserves(rpc_client, &invoice.invoicer);

        let params = PayInvoiceParams {
            discriminator: [104, 6, 62, 239, 197, 206, 208, 220],
            amount_vsol: invoice.balance_outstanding,
        };

        let accounts = vec![
            AccountMeta::new_readonly(invoice.invoicer, false),
            AccountMeta::new(invoice.invoice, false),
            AccountMeta::new(*vlp_ata, false),
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new(vlp_reserve, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ];

        ixs.push(Instruction::new_with_borsh(
            INVOICER_PROGRAM,
            &params,
            accounts,
        ));
    }
}

fn pay_invoices(rpc_client: &RpcClient, payer: &Keypair, invoices: Vec<Invoice>, auto: bool) {
    let total_vlp = invoices.iter().map(|i| i.balance_outstanding).sum::<u64>();

    let payer_pk = payer.pubkey();

    let vlp_ata = get_associated_token_address(&payer_pk, &VLP_MINT);

    let mut ixs = Vec::with_capacity(invoices.len() + 2);

    swap_sol_for_vlp(
        &mut ixs,
        rpc_client,
        &payer_pk,
        &vlp_ata,
        total_vlp,
    );

    pay(&mut ixs, rpc_client, &invoices, &payer_pk, &vlp_ata);

    if !auto {
        println!("Proceed to payment?");
        let mut response = String::new();
        std::io::stdin()
            .read_line(&mut response)
            .expect("Failed to get input");
        if !response.to_lowercase().starts_with("y") {
            return;
        }
    }

    for chunk in ixs.chunks(5) {
        let tx = Transaction::new_signed_with_payer(
            chunk,
            Some(&payer_pk),
            &vec![payer],
            rpc_client
                .get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
                .unwrap_or_else(|e| {
                    eprintln!(
                        "Cannot get a recent blockhash from RPC {}: {}\n\
                         This transaction was not sent. Try again, or use another RPC with --rpc.",
                        rpc_client.url(),
                        e
                    );
                    std::process::exit(1);
                })
                .0,
        );
        match rpc_client.send_and_confirm_transaction_with_spinner(&tx) {
            Ok(sig) => {
                println!("{}", sig);
            }
            Err(e) => {
                panic!("Tx {} failed: {}", tx.get_signature(), e);
            }
        }
    }
}

fn main() {
    let args = Args::parse();

    let rpc_client = RpcClient::new(args.rpc);

    let vote_account: Pubkey =
        Pubkey::from_str(&args.vote_account).expect("Invalid vote account address");

    let invoices = get_invoices(&rpc_client, &vote_account);

    if invoices.is_empty() {
        println!("No invoice to pay");
        return;
    }

    for i in &invoices {
        println!(
            "Epoch {} ({}): {} VLP",
            i.epoch,
            i.invoicer,
            i.balance_outstanding as f64 / 1e9
        );
    }

    if let Some(payer) = args.payer {
        let payer = solana_keypair::read_keypair_file(payer).unwrap_or_else(|e| {
            panic!("Could not read keypair: {}", e);
        });

        pay_invoices(&rpc_client, &payer, invoices, args.auto);
    }
}
