
use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    innertube::InnerTube,
    parse::{self, find, find_all, str_at, text},
};

const MAX_ACCOUNTS: u32 = 10;

const INDEX_NOTE: &str = "# tunebox-authuser: ";
const PAGE_NOTE: &str = "# tunebox-pageid: ";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub index: u32,
    pub page_id: Option<String>,
}

impl Identity {
    pub(crate) fn parse(text: &str) -> Self {
        let mut identity = Self::default();
        for line in text.lines().map(|line| line.trim_end_matches('\r')) {
            if let Some(index) = line.strip_prefix(INDEX_NOTE).and_then(|index| index.trim().parse().ok()) {
                identity.index = index;
            } else if let Some(page) = line.strip_prefix(PAGE_NOTE).map(str::trim).filter(|page| !page.is_empty()) {
                identity.page_id = Some(page.to_owned());
            }
        }
        identity
    }

    pub(crate) fn notes(&self) -> String {
        let mut notes = format!("{INDEX_NOTE}{}\n", self.index);
        if let Some(page) = &self.page_id {
            notes.push_str(&format!("{PAGE_NOTE}{page}\n"));
        }
        notes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub identity: Identity,
    pub name: String,
    pub email: Option<String>,
    pub handle: Option<String>,
    google: String,
}

impl Account {
    pub fn label(&self) -> String {
        let of = self.email.as_deref().or(self.handle.as_deref());
        match (of, &self.identity.page_id) {
            (Some(email), None) => format!("{} · {email}", self.name),
            (Some(email), Some(_)) => format!("{} · brand account of {email}", self.name),
            (None, _) => self.name.clone(),
        }
    }

    pub fn same_as(&self, other: &Account) -> bool {
        self.google == other.google && self.identity.page_id == other.identity.page_id
    }
}

pub(crate) async fn list(api: &InnerTube) -> Result<Vec<Account>> {
    let mut accounts: Vec<Account> = Vec::new();
    for index in 0..MAX_ACCOUNTS {
        let identity = Identity { index, page_id: None };
        let page = match api.call_as_identity(&identity, "account/accounts_list", json!({})).await {
            Ok(page) => page,
            Err(err) if index == 0 => return Err(err),
            Err(err) => {
                tracing::debug!("account {index}: {err:#}");
                break;
            }
        };
        let found = parse(&page, index);
        let known = found.first().is_none_or(|first| accounts.iter().any(|account| account.google == first.google));
        if known {
            break;
        }
        accounts.extend(found);
    }
    Ok(accounts)
}

pub(crate) async fn current(api: &InnerTube) -> Result<Option<Account>> {
    let Some(identity) = api.identity() else { return Ok(None) };
    let page = api.call("account/accounts_list", json!({})).await?;
    let accounts = parse(&page, identity.index);
    Ok(accounts.into_iter().find(|account| account.identity == identity))
}

fn parse(page: &Value, index: u32) -> Vec<Account> {
    let mut sections = Vec::new();
    find_all(page, "accountSectionListRenderer", &mut sections);
    let mut accounts = Vec::new();
    for section in sections {
        let email = find(&section["header"], "googleAccountHeaderRenderer")
            .map(|header| text(&header["email"]))
            .filter(|email| !email.is_empty());
        let mut items = Vec::new();
        find_all(&section["contents"], "accountItem", &mut items);
        let mut google = email.clone();
        for item in items {
            let tokens = item.pointer("/serviceEndpoint/selectActiveIdentityEndpoint/supportedTokens");
            let tokens = tokens.map_or(&[][..], parse::list);
            let page_id = tokens.iter().find_map(|token| str_at(token, "/pageIdToken/pageId")).map(str::to_owned);
            let gaia = tokens.iter().find_map(|token| str_at(token, "/accountStateToken/obfuscatedGaiaId"));
            if google.is_none() && page_id.is_none() {
                google = gaia.map(str::to_owned);
            }
            let name = text(&item["accountName"]);
            if item["isDisabled"].as_bool() == Some(true) || name.is_empty() {
                continue;
            }
            let handle = Some(text(&item["channelHandle"])).filter(|handle| !handle.is_empty());
            accounts.push(Account {
                identity: Identity { index, page_id },
                name,
                email: email.clone(),
                handle,
                google: String::new(),
            });
        }
        let google = google.unwrap_or_else(|| format!("account {index}"));
        for account in accounts.iter_mut().filter(|account| account.google.is_empty()) {
            account.google = google.clone();
        }
    }
    accounts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/accounts_list.json")).unwrap()
    }

    fn item(name: &str, handle: &str, gaia: &str, page_id: Option<&str>, disabled: bool) -> Value {
        let mut tokens = vec![json!({ "accountStateToken": { "hasChannel": true, "obfuscatedGaiaId": gaia } })];
        if let Some(page_id) = page_id {
            tokens.push(json!({ "pageIdToken": { "pageId": page_id } }));
        }
        json!({ "accountItem": {
            "accountName": { "runs": [{ "text": name }] },
            "channelHandle": { "runs": [{ "text": handle }] },
            "isDisabled": disabled,
            "serviceEndpoint": { "selectActiveIdentityEndpoint": { "supportedTokens": tokens } }
        } })
    }

    #[test]
    fn reads_the_google_account_with_its_address() {
        let accounts = parse(&fixture(), 1);
        assert_eq!(accounts.len(), 1);
        let account = &accounts[0];
        assert_eq!(account.identity, Identity { index: 1, page_id: None });
        assert_eq!(account.name, "Ada Example");
        assert_eq!(account.email.as_deref(), Some("ada@example.com"));
        assert_eq!(account.handle.as_deref(), Some("@adaexample"));
        assert_eq!(account.label(), "Ada Example · ada@example.com");
    }

    #[test]
    fn reads_brand_accounts_and_leaves_out_disabled_ones() {
        let page = json!({ "actions": [{ "accountSectionListRenderer": {
            "header": { "googleAccountHeaderRenderer": { "email": { "runs": [{ "text": "ada@example.com" }] } } },
            "contents": [{ "accountItemSectionRenderer": { "contents": [
                item("Ada Example", "@ada", "100000000000000000001", None, false),
                item("Ada's Band", "@adasband", "100000000000000000002", Some("100000000000000000099"), false),
                item("Old Channel", "", "100000000000000000003", Some("100000000000000000098"), true),
            ] } }]
        } }] });
        let accounts = parse(&page, 2);
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1].identity, Identity { index: 2, page_id: Some("100000000000000000099".into()) });
        assert_eq!(accounts[1].label(), "Ada's Band · brand account of ada@example.com");
        assert!(accounts[0].same_as(&parse(&page, 0)[0]), "the same account at another index");
        assert!(!accounts[0].same_as(&accounts[1]));
    }

    #[test]
    fn without_an_address_the_google_account_tells_accounts_apart() {
        let page = |gaia: &str| {
            json!({ "accountSectionListRenderer": { "contents": [{ "accountItemSectionRenderer": { "contents": [
                item("Ada Example", "@ada", gaia, None, false),
                item("Ada's Band", "", "100000000000000000002", Some("100000000000000000099"), false),
            ] } }] } })
        };
        let first = parse(&page("100000000000000000001"), 0);
        let other = parse(&page("100000000000000000005"), 1);
        assert_eq!(first[0].label(), "Ada Example · @ada");
        assert_eq!(first[1].label(), "Ada's Band");
        assert!(first[1].same_as(&parse(&page("100000000000000000001"), 1)[1]));
        assert!(!first[0].same_as(&other[0]));
        assert!(!first[1].same_as(&other[1]), "brand accounts of another Google account");
    }

    #[test]
    fn a_signed_out_page_has_no_accounts() {
        assert!(parse(&json!({ "actions": [{ "getMultiPageMenuAction": { "menu": {} } }] }), 5).is_empty());
    }

    #[test]
    fn the_account_is_kept_in_the_cookies_file() {
        let brand = Identity { index: 2, page_id: Some("100000000000000000099".into()) };
        assert_eq!(Identity::parse(&format!("# Netscape HTTP Cookie File\n{}", brand.notes())), brand);
        let first = Identity::default();
        assert_eq!(first.notes(), "# tunebox-authuser: 0\n");
        assert_eq!(Identity::parse(&first.notes()), first);
        assert_eq!(Identity::parse("# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSID\tx\n"), first);
    }
}
