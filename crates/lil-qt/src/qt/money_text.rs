//! `MoneyText`, a QML singleton of module `Lilith.Core`: [`crate::money::money_text`] for QML.
//! `MoneyText.text(amount, currency)` takes the core's decimal string and its currency code and
//! gives "1,234.57 USD" (owner decision 2026-10-29); `MoneyText.count(value)` gives a count
//! with thousands separators, "412,000" ([`crate::counts::count_text`]). The panels show every
//! amount and count through it; exports keep the exact values.

use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        type MoneyText = super::MoneyTextRust;

        /// `amount` in `currency`, as the apps show it.
        #[qinvokable]
        fn text(self: &MoneyText, amount: &QString, currency: &QString) -> QString;

        /// `value`, a count, with thousands separators.
        #[qinvokable]
        fn count(self: &MoneyText, value: &QString) -> QString;
    }
}

#[derive(Default)]
pub struct MoneyTextRust;

impl qobject::MoneyText {
    fn text(&self, amount: &QString, currency: &QString) -> QString {
        QString::from(&crate::money::money_text(
            &amount.to_string(),
            &currency.to_string(),
        ))
    }

    fn count(&self, value: &QString) -> QString {
        QString::from(&crate::counts::count_text(&value.to_string()))
    }
}
