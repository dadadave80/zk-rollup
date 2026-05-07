//! Print the bytes32 verification key for the STF program. The deploy script reads this
//! into `Rollup`'s `programVKey` immutable so the on-chain verifier knows which program
//! produced a proof.

use anyhow::Result;
use sp1_script::vkey_bytes32;

fn main() -> Result<()> {
    println!("{}", vkey_bytes32()?);
    Ok(())
}
