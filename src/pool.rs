//! `ax pool`: name a group of accounts that rotate among themselves. A pool
//! has one login per tool, kept apart from the default ones and from other
//! pools'.

use anyhow::{Result, bail};

use crate::login::Login;
use crate::paths;
use crate::store::{self, Provider, Roster, StoreLock};

pub fn add(pool: &str, identifiers: &[String]) -> Result<()> {
    let pool = store::validate_alias(pool)?;
    if identifiers.is_empty() {
        bail!("name at least one account to add to {pool}");
    }

    let _lock = StoreLock::acquire()?;
    let mut roster = Roster::load()?;
    let mut numbers = Vec::new();
    for identifier in identifiers {
        numbers.push(roster.find(identifier)?.number);
    }

    let members = roster.pools.entry(pool.clone()).or_default();
    for number in numbers {
        if !members.contains(&number) {
            members.push(number);
        }
    }
    roster.save()?;
    println!("Pool {pool} has {} account(s).", roster.pool(&pool)?.len());
    Ok(())
}

pub fn remove(pool: &str, identifiers: &[String]) -> Result<()> {
    let _lock = StoreLock::acquire()?;
    let mut roster = Roster::load()?;
    roster.pool(pool)?;

    let mut numbers = Vec::new();
    for identifier in identifiers {
        numbers.push(roster.find(identifier)?.number);
    }
    let members = roster.pools.get_mut(pool).expect("the pool was just found");
    members.retain(|it| !numbers.contains(it));

    if identifiers.is_empty() || members.is_empty() {
        roster.pools.remove(pool);
        println!("Removed pool {pool}.");
    } else {
        println!("Pool {pool} has {} account(s).", members.len());
    }
    roster.save()
}

pub fn list() -> Result<()> {
    let roster = Roster::load()?;
    if roster.pools.is_empty() {
        println!("No pools yet — make one with `{} pool add <name> <account>...`.", paths::invoked_as());
        return Ok(());
    }

    for pool in roster.pools.keys() {
        println!("{pool}");
        for provider in Provider::ALL {
            let live = Login::of_pool(pool, provider).live_account(&roster)?;
            for member in roster.members_of(pool, provider)? {
                let marker = if live.as_ref().is_some_and(|it| it.number == member.number) { "*" } else { " " };
                println!("{marker} {}. {} [{}]", member.number, member.email, provider.program());
            }
        }
    }
    Ok(())
}
