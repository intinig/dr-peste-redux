#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affix {
    Prefix,
    Suffix,
}

/// Rarity as reported by the in-game clipboard "Rarity:" line.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Rarity {
    Normal,
    Magic,
    Rare,
    Unique,
    Currency,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedItem {
    pub rarity: Rarity,
    pub name: String,
    pub base_type: Option<String>,
    pub item_class: Option<String>,
    pub item_level: Option<u32>,
    pub quality: Option<u32>,
    pub corrupted: bool,
    pub energy_shield: Option<u32>,
    pub armour: Option<u32>,
    pub evasion: Option<u32>,
    pub implicits: Vec<ItemStat>,
    pub enchants: Vec<ItemStat>,
    pub runes: Vec<ItemStat>,
    pub explicits: Vec<ItemStat>,
}

/// One stat line from the clipboard, with the first numeric roll extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemStat {
    pub raw: String,
    pub value: Option<f64>,
    /// Prefix/Suffix when known (Advanced-Mode `{ … Modifier … }` annotation);
    /// `None` for implicit/enchant/rune lines and for basic-clipboard pastes.
    pub affix: Option<Affix>,
    /// Affix tier `N` from a `{ … (Tier: N) … }` annotation (1 = best). `None`
    /// for basic clipboards and for continuation lines of a hybrid block.
    pub tier: Option<u8>,
}

fn is_separator(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '-')
}

/// Extracts the first signed decimal number from a stat line, e.g.
/// "+7 to Level of all Spell Skills" -> 7.0, "12.5% increased ..." -> 12.5.
pub fn first_number(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit()
            || ((c == '-' || c == '+')
                && i + 1 < bytes.len()
                && (bytes[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            if c == '-' || c == '+' {
                i += 1;
            }
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_digit() || bytes[i] as char == '.')
            {
                i += 1;
            }
            return s[start..i].trim_start_matches('+').parse::<f64>().ok();
        }
        i += 1;
    }
    None
}

/// Reads the integer after a "Label:" prefix on the matching line.
fn labeled_u32(lines: &[&str], label: &str) -> Option<u32> {
    lines
        .iter()
        .find(|l| l.starts_with(label))
        .and_then(|l| first_number(l))
        .map(|n| n as u32)
}

/// True for lines that are headers/properties/requirements/annotations rather
/// than item mods.
fn is_meta_line(l: &str) -> bool {
    const PREFIXES: [&str; 15] = [
        "Item Class:",
        "Rarity:",
        "Requirements:",
        "Requires:",
        "Level:",
        "Item Level:",
        "Quality:",
        "Sockets:",
        "Stack Size:",
        "Energy Shield:",
        "Armour:",
        "Evasion Rating:",
        "Str", // Strength/Dexterity/Intelligence attribute requirements
        "Dex",
        "Int",
    ];
    l.is_empty()
        || is_separator(l)
        || l == "Corrupted"
        || l == "Unidentified"
        || l.starts_with('{') // Advanced Mode "{ ... Modifier (Tier: N) ... }" annotations
        || PREFIXES.iter().any(|p| l.starts_with(p))
}

/// Strips a trailing parenthetical tag like " (implicit)" and returns
/// (clean_text, tag). Tag is lowercased; None if no recognized tag.
fn split_tag(l: &str) -> (String, Option<String>) {
    const KNOWN_TAGS: [&str; 6] = [
        "implicit",
        "enchant",
        "rune",
        "crafted",
        "fractured",
        "augmented",
    ];
    if let Some(open) = l.rfind('(') {
        if l.ends_with(')') {
            let tag = l[open + 1..l.len() - 1].to_lowercase();
            if KNOWN_TAGS.contains(&tag.as_str()) {
                let clean = l[..open].trim().to_string();
                return (clean, Some(tag));
            }
        }
    }
    (l.to_string(), None)
}

/// Extracts `N` from an Advanced-Mode annotation line like
/// `{ Prefix Modifier "Oppressor's" (Tier: 2) … }`. `None` if absent/unparseable.
fn parse_tier(annotation: &str) -> Option<u8> {
    let after = annotation.split("(Tier:").nth(1)?;
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Parses the PoE2 clipboard format. Returns None if no "Rarity:" line or no
/// name line is present.
pub fn parse(text: &str) -> Option<ParsedItem> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let idx = lines.iter().position(|l| l.starts_with("Rarity:"))?;

    let rarity_str = lines[idx].trim_start_matches("Rarity:").trim();
    let rarity = match rarity_str {
        "Normal" => Rarity::Normal,
        "Magic" => Rarity::Magic,
        "Rare" => Rarity::Rare,
        "Unique" => Rarity::Unique,
        "Currency" => Rarity::Currency,
        other => Rarity::Other(other.to_string()),
    };

    let name = lines
        .get(idx + 1)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty() && !is_separator(s))?;

    let base_type = lines
        .get(idx + 2)
        .filter(|s| !s.is_empty() && !is_separator(s))
        .map(|s| s.to_string());

    let item_class = lines
        .iter()
        .find(|l| l.starts_with("Item Class:"))
        .map(|l| l.trim_start_matches("Item Class:").trim().to_string())
        .filter(|s| !s.is_empty());
    let item_level = labeled_u32(&lines, "Item Level:");
    let quality = labeled_u32(&lines, "Quality:");
    let corrupted = lines.contains(&"Corrupted");
    let energy_shield = labeled_u32(&lines, "Energy Shield:");
    let armour = labeled_u32(&lines, "Armour:");
    let evasion = labeled_u32(&lines, "Evasion Rating:");

    let mut implicits = Vec::new();
    let mut enchants = Vec::new();
    let mut runes = Vec::new();
    let mut explicits = Vec::new();

    let mut current_affix: Option<Affix> = None;
    let mut current_tier: Option<u8> = None;
    for (i, raw_line) in lines.iter().enumerate() {
        if i == idx || i == idx + 1 || i == idx + 2 {
            continue; // rarity, name, base type
        }
        // Advanced-Mode affix annotations set the type for the next stat line(s).
        if raw_line.starts_with('{') {
            let lower = raw_line.to_lowercase();
            current_affix = if lower.contains("prefix modifier") {
                Some(Affix::Prefix)
            } else if lower.contains("suffix modifier") {
                Some(Affix::Suffix)
            } else {
                None // implicit/enchant/rune/other block — not a prefix/suffix
            };
            current_tier = parse_tier(raw_line);
            continue;
        }
        if is_meta_line(raw_line) {
            current_affix = None; // separators/properties break an affix block
            current_tier = None;
            continue;
        }
        let (clean, tag) = split_tag(raw_line);
        let stat = ItemStat {
            value: first_number(&clean),
            raw: clean,
            affix: None,
            tier: None,
        };
        match tag.as_deref() {
            Some("implicit") => implicits.push(stat),
            Some("enchant") => enchants.push(stat),
            Some("rune") => runes.push(stat),
            _ => {
                let mut s = stat;
                s.affix = current_affix.take(); // one slot per { … } block; hybrid continuation lines get None
                s.tier = current_tier.take(); // one tier per { … } block, like affix
                explicits.push(s);
            }
        }
    }

    Some(ParsedItem {
        rarity,
        name,
        base_type,
        item_class,
        item_level,
        quality,
        corrupted,
        energy_shield,
        armour,
        evasion,
        implicits,
        enchants,
        runes,
        explicits,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Craftability {
    pub filled_prefixes: u8,
    pub filled_suffixes: u8,
    pub open_prefixes: u8,
    pub open_suffixes: u8,
    /// Number of filled prefix+suffix explicit mods (the craftability-filter key).
    pub explicit_count: u8,
}

impl ParsedItem {
    /// Prefix/suffix slot usage, derived from Advanced-Mode affix tags.
    /// Returns `None` when no explicit carries an affix tag (basic clipboard),
    /// so callers can fall back to affix-content-only pricing.
    pub fn craftability(&self) -> Option<Craftability> {
        if !self.explicits.iter().any(|s| s.affix.is_some()) {
            return None;
        }
        let filled_prefixes = self
            .explicits
            .iter()
            .filter(|s| s.affix == Some(Affix::Prefix))
            .count() as u8;
        let filled_suffixes = self
            .explicits
            .iter()
            .filter(|s| s.affix == Some(Affix::Suffix))
            .count() as u8;
        Some(Craftability {
            filled_prefixes,
            filled_suffixes,
            open_prefixes: 3u8.saturating_sub(filled_prefixes),
            open_suffixes: 3u8.saturating_sub(filled_suffixes),
            explicit_count: filled_prefixes + filled_suffixes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIQUE: &str = "Item Class: One Hand Swords\r\nRarity: Unique\r\nThe Dancing Dervish\r\nScimitar\r\n--------\r\nLevel: 16\r\n";
    const CURRENCY: &str = "Item Class: Stackable Currency\nRarity: Currency\nDivine Orb\n--------\nStack Size: 1/10\n";
    const RARE: &str =
        "Item Class: Body Armours\nRarity: Rare\nCorpse Bramble\nVaal Regalia\n--------\n";

    #[test]
    fn parses_unique_with_base() {
        let p = parse(UNIQUE).unwrap();
        assert_eq!(p.rarity, Rarity::Unique);
        assert_eq!(p.name, "The Dancing Dervish");
        assert_eq!(p.base_type.as_deref(), Some("Scimitar"));
    }

    #[test]
    fn parses_currency_without_base() {
        let p = parse(CURRENCY).unwrap();
        assert_eq!(p.rarity, Rarity::Currency);
        assert_eq!(p.name, "Divine Orb");
        assert_eq!(p.base_type, None);
    }

    #[test]
    fn parses_rare_name_and_base() {
        let p = parse(RARE).unwrap();
        assert_eq!(p.rarity, Rarity::Rare);
        assert_eq!(p.name, "Corpse Bramble");
        assert_eq!(p.base_type.as_deref(), Some("Vaal Regalia"));
    }

    #[test]
    fn returns_none_without_rarity_line() {
        assert!(parse("just some text\nnothing here").is_none());
    }

    const RARE_STAFF: &str = "Item Class: Staves\nRarity: Rare\nBramble Bite\nExpert Crackling Staff\n--------\nQuality: +20% (augmented)\n--------\nItem Level: 82\n--------\n+7 to Level of all Spell Skills\n--------\nCorrupted\n";

    const RARE_RING: &str = "Item Class: Rings\nRarity: Rare\nWoe Coil\nSapphire Ring\n--------\nRequirements:\nLevel: 60\n--------\n+25 to maximum Mana (implicit)\n--------\n+40 to maximum Life\n+32% to Fire Resistance\n+18% to Lightning Resistance\n+12% increased Rarity of Items found (rune)\n--------\nItem Level: 80\n";

    const REQS_AND_PARENS: &str = "Item Class: Rings\nRarity: Rare\nFoo\nSapphire Ring\n--------\nRequirements:\nLevel: 60\nStr: 30\nDex: 20\nInt: 40\n--------\n25% increased Spell Damage (gained from something)\n--------\n";

    #[test]
    fn excludes_attribute_reqs_and_keeps_untagged_parentheses() {
        let p = parse(REQS_AND_PARENS).unwrap();
        assert!(p.explicits.iter().all(|s| !s.raw.starts_with("Str")
            && !s.raw.starts_with("Dex")
            && !s.raw.starts_with("Int")));
        let spell = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Spell Damage"))
            .unwrap();
        assert!(spell.raw.ends_with(')'));
        assert_eq!(spell.value, Some(25.0));
    }

    #[test]
    fn classifies_mods_by_section_tag() {
        let p = parse(RARE_RING).unwrap();
        assert_eq!(p.implicits.len(), 1);
        assert_eq!(p.implicits[0].value, Some(25.0));
        assert_eq!(p.runes.len(), 1);
        assert_eq!(p.runes[0].value, Some(12.0));
        // life + 2 resists, rune line excluded, implicit excluded
        assert_eq!(p.explicits.len(), 3);
        let fire = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Fire Resistance"))
            .unwrap();
        assert_eq!(fire.value, Some(32.0));
    }

    #[test]
    fn parses_scalar_properties() {
        let p = parse(RARE_STAFF).unwrap();
        assert_eq!(p.rarity, Rarity::Rare);
        assert_eq!(p.name, "Bramble Bite");
        assert_eq!(p.base_type.as_deref(), Some("Expert Crackling Staff"));
        assert_eq!(p.item_class.as_deref(), Some("Staves"));
        assert_eq!(p.item_level, Some(82));
        assert_eq!(p.quality, Some(20));
        assert!(p.corrupted);
    }

    const RARE_BOOTS_ADVANCED: &str = "Item Class: Boots\nRarity: Rare\nKraken Slippers\nSandsworn Sandals\n--------\nEnergy Shield: 78\n--------\nRequires: Level 75, 101 Int\n--------\nSockets: S \n--------\nItem Level: 83\n--------\n+18% to Cold Resistance (rune)\n--------\n{ Prefix Modifier \"Hellion's\" (Tier: 1) — Speed }\n35% increased Movement Speed\n{ Suffix Modifier \"of the Maelstrom\" (Tier: 3) — Elemental, Lightning, Resistance }\n+34(31-35)% to Lightning Resistance\n{ Suffix Modifier \"of Magma\" (Tier: 2) — Elemental, Fire, Resistance }\n+39(36-40)% to Fire Resistance\n{ Suffix Modifier \"of Archaeology\" (Tier: 1) }\n16(15-18)% increased Rarity of Items found\n";

    #[test]
    fn parses_advanced_mode_boots() {
        let p = parse(RARE_BOOTS_ADVANCED).unwrap();
        assert_eq!(p.base_type.as_deref(), Some("Sandsworn Sandals"));
        assert_eq!(p.item_level, Some(83));
        assert_eq!(p.runes.len(), 1, "rune mod should be captured");
        let raws: Vec<&str> = p.explicits.iter().map(|s| s.raw.as_str()).collect();
        assert!(
            raws.iter().any(|r| r.contains("increased Movement Speed")),
            "{raws:?}"
        );
        let rarity = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Rarity of Items found"))
            .expect("rarity mod present");
        // value is the current roll (16), not affected by the (15-18) range
        assert_eq!(rarity.value, Some(16.0));
        // Advanced-Mode annotation + property lines must NOT leak as mods
        assert!(
            !raws.iter().any(|r| r.starts_with('{')),
            "annotation leaked: {raws:?}"
        );
        assert!(
            !raws.iter().any(|r| r.contains("Energy Shield")),
            "{raws:?}"
        );
        assert!(!raws.iter().any(|r| r.contains("Requires")), "{raws:?}");
        assert_eq!(p.energy_shield, Some(78));
    }

    #[test]
    fn parses_evasion_rating_label() {
        let item = "Item Class: Boots\nRarity: Rare\nFoo\nLeather Boots\n--------\nEvasion Rating: 320\n--------\nItem Level: 80\n--------\n+40 to maximum Life\n";
        let p = parse(item).unwrap();
        assert_eq!(p.evasion, Some(320));
        // the "Evasion Rating:" property line is not treated as a mod
        assert!(p.explicits.iter().all(|s| !s.raw.contains("Evasion")));
    }

    #[test]
    fn craftability_of_advanced_boots() {
        let p = parse(RARE_BOOTS_ADVANCED).unwrap();
        let c = p.craftability().expect("advanced-mode tags present");
        assert_eq!(c.filled_prefixes, 1); // 35% Movement Speed
        assert_eq!(c.filled_suffixes, 3); // 2 resists + rarity
        assert_eq!(c.open_prefixes, 2); // the two empty prefixes
        assert_eq!(c.open_suffixes, 0);
        assert_eq!(c.explicit_count, 4);
        // affix tags landed on the right explicits
        let ms = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Movement Speed"))
            .unwrap();
        assert_eq!(ms.affix, Some(Affix::Prefix));
        let rarity = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Rarity of Items found"))
            .unwrap();
        assert_eq!(rarity.affix, Some(Affix::Suffix));
    }

    // One prefix BLOCK with two stat lines (a hybrid affix) + one suffix block.
    const RARE_HYBRID: &str = "Item Class: Body Armours\nRarity: Rare\nHybrid Test\nVaal Regalia\n--------\nItem Level: 80\n--------\n{ Prefix Modifier \"Of the Bear\" (Tier: 1) }\n+50 to maximum Life\n+30 to maximum Mana\n{ Suffix Modifier \"of Magma\" (Tier: 2) }\n+40% to Fire Resistance\n";

    #[test]
    fn hybrid_affix_counts_as_one_block() {
        let p = parse(RARE_HYBRID).unwrap();
        // Both hybrid stat lines are kept as query filters...
        assert_eq!(p.explicits.len(), 3); // life, mana, fire res
                                          // ...but only the first carries the prefix tag (one block = one slot).
        let life = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("maximum Life"))
            .unwrap();
        let mana = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("maximum Mana"))
            .unwrap();
        assert_eq!(life.affix, Some(Affix::Prefix));
        assert_eq!(mana.affix, None); // continuation line of the same block
        let c = p.craftability().expect("advanced-mode tags present");
        assert_eq!(c.filled_prefixes, 1);
        assert_eq!(c.open_prefixes, 2);
        assert_eq!(c.filled_suffixes, 1);
        assert_eq!(c.explicit_count, 2); // 1 prefix block + 1 suffix block
    }

    #[test]
    fn captures_tier_from_advanced_annotation() {
        // RARE_BOOTS_ADVANCED already exists in this module: Hellion's (Tier 1)
        // movement speed, of the Maelstrom (Tier 3) lightning res, of Magma (Tier 2)
        // fire res, of Archaeology (Tier 1) rarity.
        let p = parse(RARE_BOOTS_ADVANCED).unwrap();
        let ms = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Movement Speed"))
            .unwrap();
        let light = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Lightning Resistance"))
            .unwrap();
        let fire = p
            .explicits
            .iter()
            .find(|s| s.raw.contains("Fire Resistance"))
            .unwrap();
        assert_eq!(ms.tier, Some(1));
        assert_eq!(light.tier, Some(3));
        assert_eq!(fire.tier, Some(2));
    }

    #[test]
    fn tier_absent_on_basic_clipboard() {
        // RARE_RING has no Advanced-Mode annotations.
        let p = parse(RARE_RING).unwrap();
        assert!(p.explicits.iter().all(|s| s.tier.is_none()));
    }

    #[test]
    fn craftability_none_for_basic_clipboard() {
        // RARE_RING has no `{ … Modifier … }` annotations → no affix tags.
        let p = parse(RARE_RING).unwrap();
        assert!(p.explicits.iter().all(|s| s.affix.is_none()));
        assert!(p.craftability().is_none());
    }
}
