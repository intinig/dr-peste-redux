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

/// True for lines that are headers/properties/requirements rather than mods.
fn is_meta_line(l: &str) -> bool {
    const PREFIXES: [&str; 11] = [
        "Item Class:",
        "Rarity:",
        "Requirements:",
        "Level:",
        "Item Level:",
        "Quality:",
        "Sockets:",
        "Stack Size:",
        "Str",
        "Dex",
        "Int", // attribute reqs
    ];
    l.is_empty()
        || is_separator(l)
        || l == "Corrupted"
        || l == "Unidentified"
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

    let mut implicits = Vec::new();
    let mut enchants = Vec::new();
    let mut runes = Vec::new();
    let mut explicits = Vec::new();

    for (i, raw_line) in lines.iter().enumerate() {
        if i == idx || i == idx + 1 || i == idx + 2 {
            continue; // rarity, name, base type
        }
        if is_meta_line(raw_line) {
            continue;
        }
        let (clean, tag) = split_tag(raw_line);
        let stat = ItemStat {
            value: first_number(&clean),
            raw: clean,
        };
        match tag.as_deref() {
            Some("implicit") => implicits.push(stat),
            Some("enchant") => enchants.push(stat),
            Some("rune") => runes.push(stat),
            _ => explicits.push(stat),
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
        implicits,
        enchants,
        runes,
        explicits,
    })
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
}
