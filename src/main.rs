use borsh::BorshSerialize;
use borsh::de::BorshDeserialize;
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
use spl_stake_pool::find_withdraw_authority_program_address;
use spl_stake_pool::instruction::deposit_sol;
use spl_stake_pool::solana_program::pubkey;
use spl_stake_pool::solana_program::pubkey::Pubkey;
use spl_stake_pool::state::StakePool;
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

const INVOICER_BASE: Pubkey = pubkey!("vocefgUvSTg7q4ZfeTLg2RAgeYN6V7t6rNVNb3dzrh1");
const INVOICER_PROGRAM: Pubkey = pubkey!("EpoivtVh9dgWFxE6MYgF3YnobYWtZr2VfCuP7iT3N927");
const STAKE_POOL: Pubkey = pubkey!("Fu9BYC6tWBo1KMKaP3CFoKfRhqv9akmy3DuYwnCyWiyC");
const VSOL_MINT: Pubkey = pubkey!("vSoLxydx6akxyMD9XEcPvGYNGq6Nn66oqVb3UkGkei7");
const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

fn find_invoicer_address() -> Pubkey {
    Pubkey::find_program_address(&[b"invoicer", INVOICER_BASE.as_ref()], &INVOICER_PROGRAM).0
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
    amount_vsol: u64,
    balance_outstanding: u64,
}

fn parse_invoice(pk: &Pubkey, data: &Vec<u8>) -> Invoice {
    let invoicer = Pubkey::try_from(&data[8..40]).unwrap();
    // 40..72 is vote address
    let epoch = u64::from_le_bytes((&data[72..80]).try_into().unwrap());
    let amount_vsol = u64::from_le_bytes((&data[80..88]).try_into().unwrap());
    let balance_outstanding = u64::from_le_bytes((&data[88..96]).try_into().unwrap());

    Invoice {
        invoicer,
        invoice: *pk,
        epoch,
        amount_vsol,
        balance_outstanding,
    }
}

fn get_invoices(rpc_client: &RpcClient, invoicer: &Pubkey, vote_account: &Pubkey) -> Vec<Invoice> {
    let epoch_info = rpc_client
        .get_epoch_info()
        .expect("Cannot get epoch info from RPC");
    let current_epoch = epoch_info.epoch;
    let addresses = (current_epoch - 20..current_epoch)
        .map(|e| find_invoice_address(invoicer, vote_account, e))
        .collect::<Vec<Pubkey>>();

    let infos = rpc_client
        .get_multiple_accounts(addresses.as_slice())
        .expect("Cannot get invoices from RPC");
    assert_eq!(infos.len(), addresses.len());
    addresses
        .iter()
        .zip(infos.iter())
        .filter_map(|(pk, a)| {
            a.as_ref()
                .map(|a| parse_invoice(pk, &a.data))
                .filter(|i| i.balance_outstanding > 0)
        })
        .collect::<Vec<Invoice>>()
}

fn swap_sol_for_vsol(
    ixs: &mut Vec<Instruction>,
    rpc_client: &RpcClient,
    stake_pool: &StakePool,
    payer: &Pubkey,
    vsol_ata: &Pubkey,
    vsol: u64,
) {
    let vsol_balance: u64 = match rpc_client.get_token_account_balance(&vsol_ata) {
        Err(e) => {
            match e.kind {
                ErrorKind::RpcError(_) => {
                    // Assuming not existent account
                    let ix = create_associated_token_account(&payer, &payer, &VSOL_MINT, &TOKEN_PROGRAM);
                    ixs.push(ix);
                }
                _ => {
                    panic!("Cannot get VSOL balance from RPC: {}", e);
                }
            }
            0
        }
        Ok(ui) => {
            println!("VSOL balance: {} ", ui.ui_amount_string);
            ui.amount.parse::<u64>().unwrap()
        }
    };

    if vsol_balance < vsol {
        let sol: u64 = (stake_pool.total_lamports as u128 * (vsol - vsol_balance) as u128)
            .div_ceil(stake_pool.pool_token_supply as u128)
            .try_into()
            .unwrap();

        let (withdraw_authority, _) = find_withdraw_authority_program_address(&spl_stake_pool::ID, &STAKE_POOL);

        let ix = deposit_sol(
            &spl_stake_pool::ID,
            &STAKE_POOL,
            &withdraw_authority,
            &stake_pool.reserve_stake,
            &payer,
            &vsol_ata,
            &stake_pool.manager_fee_account,
            &vsol_ata,
            &stake_pool.pool_mint,
            &TOKEN_PROGRAM,
            sol,
        );

        ixs.push(ix);

        println!("Will deposit {} SOL to get {} VSOL", sol as f64 / 1e9, (vsol - vsol_balance) as f64 / 1e9);
    }
}

#[derive(BorshSerialize)]
struct PayInvoiceParams {
    discriminator: [u8; 8],
    amount_vsol: u64,
}

fn pay(ixs: &mut Vec<Instruction>, invoices: &Vec<Invoice>, payer: &Pubkey, vsol_ata: &Pubkey) {
    let vsol_reserve = get_associated_token_address(&invoices[0].invoicer, &VSOL_MINT);

    for invoice in invoices {
        let params = PayInvoiceParams {
            discriminator: [104, 6, 62, 239, 197, 206, 208, 220],
            amount_vsol: invoice.balance_outstanding,
        };

        let accounts = vec![
            AccountMeta::new_readonly(invoice.invoicer, false),
            AccountMeta::new(invoice.invoice, false),
            AccountMeta::new(*vsol_ata, false),
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new(vsol_reserve, false),
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
    let pool_data = rpc_client
        .get_account_data(&STAKE_POOL)
        .expect("Cannot get stake pool data from RPC");
    let mut pool_data = pool_data.as_slice();
    let stake_pool: StakePool =
        StakePool::deserialize(&mut pool_data).expect("Invalid stake pool data");

    let total_vsol = invoices.iter().map(|i| i.amount_vsol).sum::<u64>();

    let payer_pk = payer.pubkey();

    let vsol_ata = get_associated_token_address(&payer_pk, &VSOL_MINT);

    let mut ixs = Vec::with_capacity(invoices.len() + 2);

    swap_sol_for_vsol(
        &mut ixs,
        rpc_client,
        &stake_pool,
        &payer_pk,
        &vsol_ata,
        total_vsol,
    );

    pay(&mut ixs, &invoices, &payer_pk, &vsol_ata);

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
                .unwrap()
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

    let invoicer = find_invoicer_address();

    let invoices = get_invoices(&rpc_client, &invoicer, &vote_account);

    if invoices.is_empty() {
        println!("No invoice to pay");
        return;
    }

    for i in &invoices {
        println!(
            "Epoch {}: {} VSOL",
            i.epoch,
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
