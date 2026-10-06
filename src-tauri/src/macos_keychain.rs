//! Mutate login-keychain passwords without first reading their protected data.
//!
//! `keyring`'s macOS set/delete paths call `SecKeychainFindGenericPassword`,
//! requesting the old password even when only a replacement or deletion is
//! needed. That can prompt once for the read and again for the mutation.
//! Keep the same keychain, service/account and ACLs; let Security.framework
//! authorize only the requested mutation.

use core_foundation::data::CFData;
use security_framework::item::{
    update_item, ItemClass, ItemSearchOptions, ItemUpdateOptions, ItemUpdateValue,
};
use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};

const ITEM_NOT_FOUND: i32 = -25300;

fn query(keychain: &SecKeychain, service: &str, account: &str) -> ItemSearchOptions {
    let mut query = ItemSearchOptions::new();
    query
        .class(ItemClass::generic_password())
        .keychains(std::slice::from_ref(keychain))
        .service(service)
        .account(account);
    query
}

pub fn set_password(service: &str, account: &str, password: &str) -> keyring::Result<()> {
    let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
        .map_err(keyring::macos::decode_error)?;
    let mut update = ItemUpdateOptions::new();
    update.set_value(ItemUpdateValue::Data(CFData::from_buffer(
        password.as_bytes(),
    )));
    match update_item(&query(&keychain, service, account), &update) {
        Ok(()) => Ok(()),
        Err(err) if err.code() == ITEM_NOT_FOUND => keychain
            .add_generic_password(service, account, password.as_bytes())
            .map_err(keyring::macos::decode_error),
        Err(err) => Err(keyring::macos::decode_error(err)),
    }
}

pub fn delete_password(service: &str, account: &str) -> keyring::Result<()> {
    let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
        .map_err(keyring::macos::decode_error)?;
    match query(&keychain, service, account).delete() {
        Ok(()) => Ok(()),
        Err(err) if err.code() == ITEM_NOT_FOUND => Ok(()),
        Err(err) => Err(keyring::macos::decode_error(err)),
    }
}
