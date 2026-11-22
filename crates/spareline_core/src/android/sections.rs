//! Batched probe scripts: the markers between their commands, and the sections of their output.

use std::collections::BTreeMap;

use crate::text;

const SECTION: &str = "@@sl:";

/// The output of a batched probe script, split into named sections, in the order they first
/// appeared.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sections {
    sections: Vec<Section>,
}

/// One section of a probe script's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// The name its marker gave, such as `props` or `imei:0`.
    pub name: String,
    /// The lines after the marker, up to the next marker, joined with `\n` and trimmed.
    pub text: String,
}

impl Sections {
    /// The text of the section `name`, or `None` when the output has no such section.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|section| section.name == name)
            .map(|section| section.text.as_str())
    }

    /// The sections, in the order their names first appeared.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The number of distinct section names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Whether the output had no marker at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Sets the section `name`. A name seen before keeps its place and takes the new text, as a
    /// JavaScript `Map` does.
    fn set(&mut self, name: String, text: String) {
        match self
            .sections
            .iter_mut()
            .find(|section| section.name == name)
        {
            Some(section) => section.text = text,
            None => self.sections.push(Section { name, text }),
        }
    }
}

/// Splits the output of a batched probe script into its sections. Each section starts with a
/// line `@@sl:<name>`, which [`section_marker`] prints before the section's command. Text before
/// the first marker belongs to no section and is dropped. A name that appears twice keeps its
/// first place and the text of its last section.
#[must_use]
pub fn split_sections(stdout: &str) -> Sections {
    let mut sections = Sections::default();
    let mut name: Option<String> = None;
    let mut lines: Vec<&str> = Vec::new();
    for line in text::lines(stdout) {
        if let Some(rest) = line.strip_prefix(SECTION) {
            if let Some(finished) = name.take() {
                sections.set(finished, text::trim(&lines.join("\n")).to_owned());
            }
            name = Some(text::trim(rest).to_owned());
            lines.clear();
        } else {
            lines.push(line);
        }
    }
    if let Some(finished) = name {
        sections.set(finished, text::trim(&lines.join("\n")).to_owned());
    }
    sections
}

/// The shell command that starts the section `name` in a batched probe script:
/// `printf '\n@@sl:<name>\n'`, with the backslashes written out for `printf` to read.
#[must_use]
pub fn section_marker(name: &str) -> String {
    format!("printf '\\n{SECTION}{name}\\n'")
}

/// The `key=value` lines the identity probe's `getprop` loop prints. A line without `=`, or that
/// starts with one, is skipped; a key seen twice takes its last value. Keys and values are
/// trimmed.
#[must_use]
pub fn parse_key_values(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text::lines(text) {
        if let Some(eq) = line.find('=')
            && eq > 0
        {
            values.insert(
                text::trim(&line[..eq]).to_owned(),
                text::trim(&line[eq + 1..]).to_owned(),
            );
        }
    }
    values
}

/// The output of `getprop gsm.sim.state`: one comma-separated state per slot, such as
/// `LOADED,ABSENT`. Empty output is no slots.
#[must_use]
pub fn parse_sim_states(text: &str) -> Vec<String> {
    let value = text::trim(text);
    if value.is_empty() {
        return Vec::new();
    }
    value
        .split(',')
        .map(|state| text::trim(state).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_split_on_marker_lines_and_trim() {
        let sections = split_sections("\n@@sl:a\none\ntwo\n\n@@sl:b\r\nthree\n");
        assert_eq!(
            sections.sections(),
            [
                Section {
                    name: "a".to_owned(),
                    text: "one\ntwo".to_owned(),
                },
                Section {
                    name: "b".to_owned(),
                    text: "three".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn text_before_the_first_marker_is_dropped() {
        let sections = split_sections("noise\n@@sl:only\nvalue");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections.get("only"), Some("value"));
        assert_eq!(split_sections("no markers at all"), Sections::default());
    }

    #[test]
    fn a_repeated_name_keeps_its_place_and_takes_the_last_text() {
        let sections = split_sections("@@sl:a\n1\n@@sl:b\n2\n@@sl:a\n3\n");
        let names: Vec<&str> = sections
            .sections()
            .iter()
            .map(|section| section.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(sections.get("a"), Some("3"));
    }

    #[test]
    fn a_marker_name_is_trimmed_and_may_be_empty() {
        let sections = split_sections("@@sl: spaced \nx\n@@sl:\ny");
        assert_eq!(sections.get("spaced"), Some("x"));
        assert_eq!(sections.get(""), Some("y"));
    }

    #[test]
    fn marker_command() {
        assert_eq!(section_marker("props"), r"printf '\n@@sl:props\n'");
    }

    #[test]
    fn key_values_skip_lines_without_a_key_and_keep_the_last_value() {
        let values = parse_key_values("a=1\n=nokey\nnoequals\n b = two = 2 \r\na=3");
        assert_eq!(values.len(), 2);
        assert_eq!(values.get("a").map(String::as_str), Some("3"));
        assert_eq!(values.get("b").map(String::as_str), Some("two = 2"));
    }

    #[test]
    fn sim_states_split_on_commas() {
        assert_eq!(parse_sim_states("LOADED,ABSENT\n"), ["LOADED", "ABSENT"]);
        assert_eq!(
            parse_sim_states(" LOADED , ,ABSENT"),
            ["LOADED", "", "ABSENT"]
        );
        assert_eq!(parse_sim_states(" \n"), Vec::<String>::new());
    }
}
