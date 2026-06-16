#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Exchange,
    StashItem,
}

impl Family {
    pub fn path(self) -> &'static str {
        match self {
            Family::Exchange => "exchange/current/overview",
            Family::StashItem => "stash/current/item/overview",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Category {
    pub slug: &'static str,
    pub type_param: &'static str,
    pub display: &'static str,
    pub family: Family,
}

use Family::{Exchange as EX, StashItem as ST};

pub const CATEGORIES: &[Category] = &[
    Category {
        slug: "currency",
        type_param: "Currency",
        display: "Currency",
        family: EX,
    },
    Category {
        slug: "fragments",
        type_param: "Fragments",
        display: "Fragments",
        family: EX,
    },
    Category {
        slug: "abyssal-bones",
        type_param: "Abyss",
        display: "Abyssal Bones",
        family: EX,
    },
    Category {
        slug: "uncut-gems",
        type_param: "UncutGems",
        display: "Uncut Gems",
        family: EX,
    },
    Category {
        slug: "lineage-support-gems",
        type_param: "LineageSupportGems",
        display: "Lineage Support Gems",
        family: EX,
    },
    Category {
        slug: "essences",
        type_param: "Essences",
        display: "Essences",
        family: EX,
    },
    Category {
        slug: "soul-cores",
        type_param: "SoulCores",
        display: "Soul Cores",
        family: EX,
    },
    Category {
        slug: "idols",
        type_param: "Idols",
        display: "Idols",
        family: EX,
    },
    Category {
        slug: "runes",
        type_param: "Runes",
        display: "Runes",
        family: EX,
    },
    Category {
        slug: "omens",
        type_param: "Ritual",
        display: "Omens",
        family: EX,
    },
    Category {
        slug: "expedition",
        type_param: "Expedition",
        display: "Expedition",
        family: EX,
    },
    Category {
        slug: "liquid-emotions",
        type_param: "Delirium",
        display: "Liquid Emotions",
        family: EX,
    },
    Category {
        slug: "breach-catalyst",
        type_param: "Breach",
        display: "Breach Catalysts",
        family: EX,
    },
    Category {
        slug: "verisium",
        type_param: "Verisium",
        display: "Verisium",
        family: EX,
    },
    Category {
        slug: "precursor-tablets",
        type_param: "PrecursorTablets",
        display: "Precursor Tablets",
        family: ST,
    },
    Category {
        slug: "unique-weapons",
        type_param: "UniqueWeapons",
        display: "Unique Weapons",
        family: ST,
    },
    Category {
        slug: "unique-armours",
        type_param: "UniqueArmours",
        display: "Unique Armours",
        family: ST,
    },
    Category {
        slug: "unique-accessories",
        type_param: "UniqueAccessories",
        display: "Unique Accessories",
        family: ST,
    },
    Category {
        slug: "unique-flasks",
        type_param: "UniqueFlasks",
        display: "Unique Flasks",
        family: ST,
    },
    Category {
        slug: "unique-charms",
        type_param: "UniqueCharms",
        display: "Unique Charms",
        family: ST,
    },
    Category {
        slug: "unique-jewels",
        type_param: "UniqueJewels",
        display: "Unique Jewels",
        family: ST,
    },
    Category {
        slug: "unique-relics",
        type_param: "UniqueSanctumRelics",
        display: "Unique Relics",
        family: ST,
    },
    Category {
        slug: "unique-tablets",
        type_param: "UniqueTablets",
        display: "Unique Tablets",
        family: ST,
    },
];

pub fn by_slug(slug: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_all_23_categories() {
        assert_eq!(CATEGORIES.len(), 23);
    }

    #[test]
    fn slugs_are_unique() {
        let mut slugs: Vec<&str> = CATEGORIES.iter().map(|c| c.slug).collect();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), 23);
    }

    #[test]
    fn lookup_resolves_tricky_types() {
        assert_eq!(by_slug("omens").unwrap().type_param, "Ritual");
        assert_eq!(by_slug("liquid-emotions").unwrap().type_param, "Delirium");
        assert_eq!(
            by_slug("unique-relics").unwrap().type_param,
            "UniqueSanctumRelics"
        );
        assert_eq!(by_slug("breach-catalyst").unwrap().type_param, "Breach");
    }

    #[test]
    fn families_use_correct_paths() {
        assert_eq!(
            by_slug("currency").unwrap().family.path(),
            "exchange/current/overview"
        );
        assert_eq!(
            by_slug("unique-weapons").unwrap().family.path(),
            "stash/current/item/overview"
        );
    }
}
