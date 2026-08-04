use std::sync::LazyLock;

use memchr::memmem::Finder;
use ruff_diagnostics::SourceMap;
use ruff_source_file::UniversalNewlineIterator;
use ruff_text_size::{TextLen, TextSize};

static FINDER: LazyLock<Finder> = LazyLock::new(|| Finder::new(b"# /// script"));

/// PEP 723 metadata as parsed from a `script` comment block.
///
/// See: <https://peps.python.org/pep-0723/>
///
/// Vendored from: <https://github.com/astral-sh/uv/blob/debe67ffdb0cd7835734100e909b2d8f79613743/crates/uv-scripts/src/lib.rs#L283>
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ScriptTag {
    /// The content of the script before the metadata block.
    prelude: String,
    /// The metadata block.
    metadata: String,
    /// Maps offsets in the extracted metadata to offsets in the original Python script.
    metadata_source_map: SourceMap,
    /// The content of the script after the metadata block.
    postlude: String,
}

impl ScriptTag {
    /// Returns the TOML contents of the metadata block.
    pub fn metadata(&self) -> &str {
        &self.metadata
    }

    /// Returns the map from extracted TOML offsets to their original script offsets.
    pub fn metadata_source_map(&self) -> &SourceMap {
        &self.metadata_source_map
    }

    /// Consumes this tag and returns its TOML together with its original-source mapping.
    pub fn into_metadata_and_source_map(self) -> (String, SourceMap) {
        (self.metadata, self.metadata_source_map)
    }

    /// Given the contents of a Python file, extract the `script` metadata block with leading
    /// comment hashes removed, any preceding shebang or content (prelude), and the remaining Python
    /// script.
    ///
    /// Given the following input string representing the contents of a Python script:
    ///
    /// ```python
    /// #!/usr/bin/env python3
    /// # /// script
    /// # requires-python = '>=3.11'
    /// # dependencies = [
    /// #   'requests<3',
    /// #   'rich',
    /// # ]
    /// # ///
    ///
    /// import requests
    ///
    /// print("Hello, World!")
    /// ```
    ///
    /// This function would return:
    ///
    /// - Preamble: `#!/usr/bin/env python3\n`
    /// - Metadata: `requires-python = '>=3.11'\ndependencies = [\n  'requests<3',\n  'rich',\n]`
    /// - Postlude: `import requests\n\nprint("Hello, World!")\n`
    ///
    /// See: <https://peps.python.org/pep-0723/>
    pub fn parse(contents: &[u8]) -> Option<Self> {
        FINDER
            .find_iter(contents)
            .find_map(|index| Self::parse_at(contents, index))
    }

    fn parse_at(contents: &[u8], index: usize) -> Option<Self> {
        // The opening pragma must be the first line, or immediately preceded by a newline.
        if !(index == 0 || matches!(contents[index - 1], b'\r' | b'\n')) {
            return None;
        }

        // Extract the preceding content.
        let prelude = std::str::from_utf8(&contents[..index]).ok()?;

        // Decode as UTF-8.
        let contents = &contents[index..];
        let contents = std::str::from_utf8(contents).ok()?;

        let mut lines =
            UniversalNewlineIterator::with_offset(contents, TextSize::try_from(index).ok()?);

        // Ensure that the first line is exactly `# /// script`.
        if lines
            .next()
            .is_none_or(|line| line.as_str() != "# /// script")
        {
            return None;
        }

        // > Every line between these two lines (# /// TYPE and # ///) MUST be a comment starting
        // > with #. If there are characters after the # then the first character MUST be a space. The
        // > embedded content is formed by taking away the first two characters of each line if the
        // > second character is a space, otherwise just the first character (which means the line
        // > consists of only a single #).
        let mut toml = vec![];

        // Extract the content that follows the metadata block.
        let mut python_script = vec![];

        while let Some(line) = lines.next() {
            // Remove the leading `#`.
            let Some(comment) = line.as_str().strip_prefix('#') else {
                python_script.push(line.as_str());
                python_script.extend(lines.map(|line| line.as_str()));
                break;
            };

            // If the line is empty, continue.
            if comment.is_empty() {
                toml.push(ScriptMetadataLine {
                    text: "",
                    source_start: line.start() + TextSize::new(1),
                    source_end: line.full_end(),
                });
                continue;
            }

            // Otherwise, the line _must_ start with ` `.
            let Some(metadata) = comment.strip_prefix(' ') else {
                python_script.push(comment);
                python_script.extend(lines.map(|line| line.as_str()));
                break;
            };

            toml.push(ScriptMetadataLine {
                text: metadata,
                source_start: line.start() + TextSize::new(2),
                source_end: line.full_end(),
            });
        }

        // Find the closing `# ///`. The precedence is such that we need to identify the _last_ such
        // line.
        //
        // For example, given:
        // ```python
        // # /// script
        // #
        // # ///
        // #
        // # ///
        // ```
        //
        // The latter `///` is the closing pragma
        let index = toml.iter().rev().position(|line| line.text == "///")?;
        let index = toml.len() - index;

        // Discard any lines after the closing `# ///`.
        //
        // For example, given:
        // ```python
        // # /// script
        // #
        // # ///
        // #
        // #
        // ```
        //
        // We need to discard the last two lines.
        toml.truncate(index - 1);

        // Reconstruct the TOML while recording where each uncommented line begins in the script.
        let mut metadata = String::new();
        let mut metadata_source_map = SourceMap::default();

        for line in &toml {
            metadata_source_map.push_marker(metadata.text_len(), line.source_start);
            metadata.push_str(line.text);
            metadata.push('\n');
        }

        if let Some(last) = toml.last() {
            metadata_source_map.push_marker(metadata.text_len(), last.source_end);
        }

        if metadata.is_empty() {
            metadata.push('\n');
        }

        let prelude = prelude.to_string();
        let postlude = python_script.join("\n") + "\n";

        Some(Self {
            prelude,
            metadata,
            metadata_source_map,
            postlude,
        })
    }
}

struct ScriptMetadataLine<'a> {
    text: &'a str,
    source_start: TextSize,
    source_end: TextSize,
}

#[cfg(test)]
mod tests {
    use ruff_text_size::{TextLen, TextRange, TextSize};

    use super::ScriptTag;

    #[test]
    fn metadata_range_accounts_for_unicode_crlf_and_empty_comments() -> Result<(), &'static str> {
        let source = "π = 1\r\n# /// script\r\n# [tool.ty.rules]\r\n#\r\n# unknown-rule = \"warn\"\r\n# ///\r\n";
        let tag = ScriptTag::parse(source.as_bytes()).ok_or("Expected valid script metadata")?;
        let metadata_start = tag
            .metadata()
            .find("unknown-rule")
            .ok_or("Expected rule in extracted metadata")?;
        let source_start = source
            .find("unknown-rule")
            .ok_or("Expected rule in original source")?;
        let metadata_range = TextRange::at(
            TextSize::try_from(metadata_start).map_err(|_| "Metadata offset exceeds TextSize")?,
            "unknown-rule".text_len(),
        );
        let source_range = TextRange::at(
            TextSize::try_from(source_start).map_err(|_| "Source offset exceeds TextSize")?,
            "unknown-rule".text_len(),
        );

        assert_eq!(
            tag.metadata_source_map().map_range(metadata_range),
            source_range
        );

        Ok(())
    }

    #[test]
    fn metadata_range_spans_multiple_lines_without_changing_toml() -> Result<(), &'static str> {
        let source = "# /// script\n# value = \"\"\"\n# first\n#\n# last\n# \"\"\"\n# ///\n";
        let tag = ScriptTag::parse(source.as_bytes()).ok_or("Expected valid script metadata")?;

        assert_eq!(tag.metadata(), "value = \"\"\"\nfirst\n\nlast\n\"\"\"\n");

        let metadata_start = tag
            .metadata()
            .find("\"\"\"")
            .ok_or("Expected opening multiline delimiter")?;
        let metadata_end = tag
            .metadata()
            .rfind("\"\"\"")
            .ok_or("Expected closing multiline delimiter")?
            + "\"\"\"".len();
        let source_start = source
            .find("\"\"\"")
            .ok_or("Expected opening delimiter in source")?;
        let source_end = source
            .rfind("\"\"\"")
            .ok_or("Expected closing delimiter in source")?
            + "\"\"\"".len();
        let metadata_range = TextRange::new(
            TextSize::try_from(metadata_start).map_err(|_| "Metadata offset exceeds TextSize")?,
            TextSize::try_from(metadata_end).map_err(|_| "Metadata offset exceeds TextSize")?,
        );
        let source_range = TextRange::new(
            TextSize::try_from(source_start).map_err(|_| "Source offset exceeds TextSize")?,
            TextSize::try_from(source_end).map_err(|_| "Source offset exceeds TextSize")?,
        );

        assert_eq!(
            tag.metadata_source_map().map_range(metadata_range),
            source_range
        );

        Ok(())
    }

    #[test]
    fn metadata_range_uses_the_selected_metadata_block() -> Result<(), &'static str> {
        let source = "# /// script invalid\n# ignored = true\n# ///\n\n# /// script\n# selected = true\n# ///\n";
        let tag = ScriptTag::parse(source.as_bytes()).ok_or("Expected a later valid block")?;
        let metadata_start = tag
            .metadata()
            .find("selected")
            .ok_or("Expected selected metadata")?;
        let source_start = source
            .find("selected")
            .ok_or("Expected selected metadata in source")?;
        let metadata_range = TextRange::at(
            TextSize::try_from(metadata_start).map_err(|_| "Metadata offset exceeds TextSize")?,
            "selected".text_len(),
        );
        let source_range = TextRange::at(
            TextSize::try_from(source_start).map_err(|_| "Source offset exceeds TextSize")?,
            "selected".text_len(),
        );

        assert_eq!(
            tag.metadata_source_map().map_range(metadata_range),
            source_range
        );

        Ok(())
    }
}
