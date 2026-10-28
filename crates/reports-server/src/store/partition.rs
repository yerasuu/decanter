//! The bucket is split by product (#1055): `decanter/` and `psport/` are two s3db stores, each with its own
//! encryption config, entities, indexes, blobs and totals. A report id starts with its product's letter, so every
//! id route reaches its partition without a lookup; a fingerprint names no product, so group reads and actions ask
//! both partitions. Lists that span both are merged here, in the order each partition's index already reads.

use anyhow::Result;

use super::read::AdminRow;
use super::write::{Change, Changed};
use super::{AdminFilter, Filter, GroupRow, Report, Store};
use crate::secrets::{is_hex_token, random_hex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    Decanter,
    Psport,
}

impl Product {
    pub const ALL: [Self; 2] = [Self::Decanter, Self::Psport];

    /// The report's `product`, the partition's directory or key prefix, and the filter value.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Decanter => "decanter",
            Self::Psport => "psport",
        }
    }

    const fn tag(self) -> char {
        match self {
            Self::Decanter => 'd',
            Self::Psport => 'p',
        }
    }

    pub fn from_name(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|product| product.name() == text)
    }

    /// The partition a report id belongs to, for a well-formed id only: the product's letter and 32 lowercase hex
    /// characters. Anything else names no partition and is answered like an unknown id.
    pub fn of_id(id: &str) -> Option<Self> {
        let mut chars = id.chars();
        let tag = chars.next()?;
        let product = Self::ALL.into_iter().find(|product| product.tag() == tag)?;
        is_hex_token(chars.as_str(), 32).then_some(product)
    }

    /// 128 random bits behind the product's letter.
    ///
    /// # Errors
    ///
    /// The operating system's random source is unavailable.
    pub fn new_id(self) -> Result<String, getrandom::Error> {
        Ok(format!("{}{}", self.tag(), random_hex()?))
    }
}

#[derive(Clone)]
pub struct Stores {
    decanter: Store,
    psport: Store,
}

impl Stores {
    pub const fn new(decanter: Store, psport: Store) -> Self {
        Self { decanter, psport }
    }

    pub const fn get(&self, product: Product) -> &Store {
        match product {
            Product::Decanter => &self.decanter,
            Product::Psport => &self.psport,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Product, &Store)> {
        Product::ALL.into_iter().map(|product| (product, self.get(product)))
    }

    /// The partition of a well-formed id; `None` for anything else.
    pub fn of_id(&self, id: &str) -> Option<&Store> {
        Product::of_id(id).map(|product| self.get(product))
    }

    /// One page of the index. Without a product both partitions are read up to the end of the page and merged by
    /// the order each index uses (latest received day, then fingerprint); a fingerprint present in both partitions
    /// shows as one row per product.
    pub async fn groups(&self, filter: &Filter, limit: u32, offset: u32) -> Result<Vec<GroupRow>> {
        if let Some(product) = filter.product {
            return self.get(product).groups(filter, limit, offset).await;
        }
        let wanted = offset.saturating_add(limit);
        let (mut rows, psport) = tokio::try_join!(
            self.decanter.groups(filter, wanted, 0),
            self.psport.groups(filter, wanted, 0)
        )?;
        rows.extend(psport);
        // Stable, so equal keys keep decanter's rows first and each partition's own order.
        rows.sort_by(|a, b| {
            b.last_received
                .cmp(&a.last_received)
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        });
        Ok(page(rows, limit, offset))
    }

    /// The visible reports of a group in every partition that holds it, newest received day first.
    pub async fn group_members(&self, fingerprint: &str, limit: u32, offset: u32) -> Result<Vec<Report>> {
        let wanted = offset.saturating_add(limit);
        let (mut members, psport) = tokio::try_join!(
            self.decanter.group_members(fingerprint, wanted, 0),
            self.psport.group_members(fingerprint, wanted, 0)
        )?;
        if psport.is_empty() {
            return Ok(page(members, limit, offset));
        }
        members.extend(psport);
        members.sort_by(|a, b| b.received_day.cmp(&a.received_day));
        Ok(page(members, limit, offset))
    }

    /// Hidden reports included, newest received day first; a product filter reads that partition only.
    pub async fn admin_list(&self, filter: &AdminFilter, limit: u32, offset: u32) -> Result<Vec<AdminRow>> {
        if let Some(product) = filter.product {
            return self.get(product).admin_list(filter, limit, offset).await;
        }
        let wanted = offset.saturating_add(limit);
        let (mut rows, psport) = tokio::try_join!(
            self.decanter.admin_list(filter, wanted, 0),
            self.psport.admin_list(filter, wanted, 0)
        )?;
        rows.extend(psport);
        rows.sort_by(|a, b| b.received_day.cmp(&a.received_day));
        Ok(page(rows, limit, offset))
    }

    /// Applies `change` to every report of the group in each partition that holds it. A failure in the second
    /// partition leaves the first one changed; repeating the action finishes it.
    ///
    /// # Errors
    ///
    /// The bucket fails, or a transaction keeps losing races.
    pub async fn change_group(&self, fingerprint: &str, change: &Change) -> Result<Changed> {
        let mut total = Changed::default();
        for (_, store) in self.iter() {
            let ids = store.group_ids(fingerprint).await?;
            total.absorb(&store.change(&ids, change).await?);
        }
        Ok(total)
    }

    #[cfg(test)]
    pub async fn count(&self) -> Result<u32> {
        Ok(self.decanter.count().await? + self.psport.count().await?)
    }

    #[cfg(test)]
    pub async fn blob_count(&self) -> Result<usize> {
        Ok(self.decanter.blob_count().await? + self.psport.blob_count().await?)
    }
}

fn page<T>(rows: Vec<T>, limit: u32, offset: u32) -> Vec<T> {
    rows.into_iter().skip(offset as usize).take(limit as usize).collect()
}

/// Two stores over `<root>/decanter` and `<root>/psport` with the fixed test key.
#[cfg(test)]
pub fn test_stores(root: &std::path::Path) -> Stores {
    let [decanter, psport] =
        Product::ALL.map(|product| super::test_store(&root.join(product.name()), s3db::LocalEngine::Redb));
    Stores::new(decanter, psport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_their_product_and_nothing_else_parses() -> Result<(), getrandom::Error> {
        let decanter = Product::Decanter.new_id()?;
        let psport = Product::Psport.new_id()?;
        assert_eq!(decanter.len(), 33);
        assert_eq!(Product::of_id(&decanter), Some(Product::Decanter));
        assert_eq!(Product::of_id(&psport), Some(Product::Psport));
        let hex = "0123456789abcdef0123456789abcdef";
        for bad in [
            hex.to_owned(),
            format!("x{hex}"),
            format!("D{hex}"),
            format!("d{}", hex.to_uppercase()),
            format!("d{hex}0"),
            format!("p{}", &hex[1..]),
            format!("d{}g", &hex[1..]),
            String::new(),
            "d".to_owned(),
            format!("é{}", &hex[2..]),
        ] {
            assert_eq!(Product::of_id(&bad), None, "{bad}");
        }
        Ok(())
    }
}
