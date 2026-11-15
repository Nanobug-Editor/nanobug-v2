use super::*;

/// Read only renderable top-level blocks from `source` in layout regression tests.
fn parse(source: &str) -> Vec<Block> {
    super::parse(source)
        .blocks
        .into_iter()
        .map(|block| block.block)
        .collect()
}

/// Returns the visible text of a run with its default appearance.
fn text(content: &str) -> Inline {
    Inline::Text {
        content: content.into(),
        style: InlineStyle::default(),
    }
}

/// Wrap expected `inlines` in a paragraph using the default alignment.
fn paragraph(inlines: Vec<Inline>) -> Block {
    Block::Paragraph(AlignedText {
        inlines,
        alignment: Alignment::Left,
    })
}

#[test]
fn parses_markdown_with_github_extensions() {
    let blocks = parse(concat!(
        "# A **big** title\n\n",
        "~~Old~~ and `a  b`\n\n",
        "3. [x] Done\n4. [ ] Later\n\n",
        "| Center | Right |\n| :--: | ---: |\n| one | two |\n\n",
        "> [!TIP]\n> Pick this.\n\n",
        "```txt\n   code\n```",
    ));
    assert!(matches!(
        &blocks[0],
        Block::Heading { level: 1, inlines, id: None, .. }
            if matches!(&inlines[1], Inline::Text { content, style }
                if content.trim_end() == "big" && style.bold)
    ));
    assert!(matches!(
        &blocks[1],
        Block::Paragraph(AlignedText { inlines, .. })
            if matches!(&inlines[0], Inline::Text { style, .. } if style.strike)
                && matches!(&inlines[2], Inline::Text { content, style }
                    if content == "a  b" && style.code)
    ));
    assert!(matches!(
        &blocks[2],
        Block::List { start: Some(3), items }
            if matches!(&items[0][0].block, Block::Paragraph(AlignedText { inlines, .. })
                if matches!(inlines[0], Inline::Checkbox(true)))
                && matches!(&items[1][0].block, Block::Paragraph(AlignedText { inlines, .. })
                    if matches!(inlines[0], Inline::Checkbox(false)))
    ));
    assert!(matches!(
        &blocks[3],
        Block::Table { headings, rows, .. }
            if headings[0].inlines == [text("Center")]
                && rows[0][1].inlines == [text("two")]
                && headings[0].alignment == Alignment::Center
                && rows[0][1].alignment == Alignment::Right
    ));
    assert!(matches!(
        &blocks[4],
        Block::Quote(contents)
            if matches!(&contents[0].block, Block::Paragraph(AlignedText { inlines, .. })
                if matches!(&inlines[0], Inline::Text { content, style }
                    if content == "Tip" && style.bold))
    ));
    assert!(matches!(
        &blocks[5],
        Block::Code { content, alignment: Alignment::Left, .. }
            if content == "   code\n"
    ));
}

#[test]
fn parses_inline_html_and_implicitly_closed_tags() {
    let blocks = parse(concat!(
        "A <strong>bold <i>word</i></strong> &amp; another<br>line.\n\n",
        "<div><h2>HTML</h2><ul><li>First<li>Second</ul>",
        "<pre>first<br>second</pre></div>",
    ));
    let Block::Paragraph(AlignedText { inlines, .. }) = &blocks[0] else {
        panic!("expected a paragraph");
    };
    assert_eq!(inlines[0], text("A "));
    assert!(matches!(&inlines[2], Inline::Text { content, style }
        if content.trim_end() == "word" && style.bold && style.italic));
    assert_eq!(inlines[3], text(" & another"));
    assert_eq!(inlines[4], Inline::Break);
    assert_eq!(inlines[5], text("line."));
    assert_eq!(
        blocks[1],
        Block::Heading {
            level: 2,
            inlines: vec![text("HTML")],
            id: None,
            alignment: Alignment::Left,
        }
    );
    assert!(matches!(
        &blocks[2],
        Block::List { start: None, items }
            if items.len() == 2
                && items[0][0].block == paragraph(vec![text("First")])
                && items[1][0].block == paragraph(vec![text("Second")])
    ));
    assert!(matches!(
        &blocks[3],
        Block::Code { content, alignment: Alignment::Left, .. }
            if content == "first\nsecond"
    ));
}

#[test]
fn omits_active_html_and_rejects_unsafe_destinations() {
    let blocks = parse(concat!(
        "<div><script>hide this</script><style>and this</style>",
        "<p hidden>hidden</p><p style='DISPLAY: none'>hidden by CSS</p>",
        "<a href='java&#x73;cript:example'>first</a> ",
        "<a href='../guide.md#intro' onclick='nothing'>second</a> ",
        "<img src='data:text/html,blocked' alt='Image'>",
        "</div>",
    ));
    let [Block::Paragraph(AlignedText { inlines, .. })] = blocks.as_slice() else {
        panic!("expected one paragraph");
    };
    assert_eq!(inlines[0], text("first "));
    assert!(matches!(&inlines[1], Inline::Text { content, style }
        if content == "second" && style.link.as_deref() == Some("../guide.md#intro")));
    assert_eq!(inlines[2], text(" "));
    assert_eq!(
        inlines[3],
        Inline::Image {
            alt: "Image".into(),
            url: None,
            link: None,
            size: ImageSize::default(),
            alignment: None,
        }
    );
}

/// Footnote references link to the standard container ID emitted by the Markdown parser.
#[test]
fn retains_generated_footnote_destination() {
    let blocks = parse("Footnote[^example].\n\n[^example]: A note.");
    assert!(matches!(
        &blocks[0],
        Block::Paragraph(AlignedText { inlines, .. })
            if inlines.iter().any(|inline| matches!(inline, Inline::Text { style, .. }
                if style.link.as_deref() == Some("#example")))
    ));
    assert!(matches!(
        &blocks[1],
        Block::Paragraph(AlignedText { inlines, .. })
            if inlines.first() == Some(&Inline::Anchor("example".into()))
    ));
}

/// Preserve the target of a linked badge and explicit destinations in static HTML.
#[test]
fn retains_linked_images_and_named_anchors() {
    let blocks = parse(concat!(
        "[![Badge](badges/build.png)](status.md#latest)\n\n",
        "<h2 id=\"Status\">Latest status</h2>\n\n",
        "<a name=\"previous-run\"></a>Previous run",
    ));
    assert_eq!(
        blocks,
        [
            paragraph(vec![Inline::Image {
                alt: "Badge".into(),
                url: Some("badges/build.png".into()),
                link: Some("status.md#latest".into()),
                size: ImageSize::default(),
                alignment: None,
            }]),
            Block::Heading {
                level: 2,
                inlines: vec![text("Latest status")],
                id: Some("Status".into()),
                alignment: Alignment::Left,
            },
            paragraph(vec![
                Inline::Anchor("previous-run".into()),
                text("Previous run")
            ]),
        ]
    );
}

/// Preserve HTML image dimensions and prefer supported inline style declarations.
#[test]
fn markdown_html_image_size_attributes_and_styles() {
    let blocks = parse(concat!(
        "<img src='a.png' width='42' height='21'>",
        "<img src='b.png' width='120' height='55' style='WIDTH: 30.5%; height: 18PX'>",
        "<img src='c.png' width='50%' height='-1' style='width: unknown'>",
        "<img src='d.png' width='NaN' height='0'>",
        "<img src='e.png' width='17px' height='29px'>",
    ));
    let [Block::Paragraph(AlignedText { inlines, .. })] = blocks.as_slice() else {
        panic!("expected inline HTML images");
    };
    let sizes = inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Image { size, .. } => Some(*size),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sizes,
        [
            ImageSize {
                width: Some(ImageWidth::Pixels(42.)),
                height: Some(21.),
            },
            ImageSize {
                width: Some(ImageWidth::Percent(30.5)),
                height: Some(18.),
            },
            ImageSize {
                width: Some(ImageWidth::Percent(50.)),
                height: None,
            },
            ImageSize {
                width: None,
                height: Some(0.),
            },
            ImageSize {
                width: Some(ImageWidth::Pixels(17.)),
                height: Some(29.),
            },
        ]
    );
}

/// Preserve the README's centered linked image and pass inherited alignment to nested content.
#[test]
fn markdown_html_alignment_is_inherited_and_can_be_overridden() {
    let blocks = parse(concat!(
        "<p align='center'><a href='guide.md'><img src='logo.png' width='128' height='128'></a></p>\n\n",
        "<div align='right'><p>right</p><center>center</center>",
        "<h2 align='right' style='text-align: left'>heading</h2>",
        "<pre>right code</pre></div>\n\n",
        "<img src='logo.png' align='right' style='display:block; margin:0 auto'>",
        "<img src='logo.png' style='margin:auto; margin-right:0'>",
    ));

    assert!(matches!(
        &blocks[0],
        Block::Paragraph(AlignedText { alignment: Alignment::Center, inlines })
            if matches!(inlines.as_slice(), [Inline::Image { alignment: None, link, .. }]
                if link.as_deref() == Some("guide.md"))
    ));
    assert!(matches!(
        &blocks[1],
        Block::Paragraph(AlignedText { alignment: Alignment::Right, inlines })
            if inlines == &[text("right")]
    ));
    assert!(matches!(
        &blocks[2],
        Block::Paragraph(AlignedText { alignment: Alignment::Center, inlines })
            if inlines == &[text("center")]
    ));
    assert!(matches!(
        &blocks[3],
        Block::Heading {
            alignment: Alignment::Left,
            ..
        }
    ));
    assert!(matches!(
        &blocks[4],
        Block::Code {
            alignment: Alignment::Right,
            ..
        }
    ));
    assert!(matches!(
        &blocks[5],
        Block::Paragraph(AlignedText { inlines, .. })
            if matches!(inlines.as_slice(),
                [Inline::Image { alignment: Some(Alignment::Center), .. },
                 Inline::Image { alignment: Some(Alignment::Right), .. }])
    ));
}

/// Retain HTML row grouping and alignment independently of each column's first cell.
#[test]
fn markdown_html_table_alignment_is_resolved_per_cell() {
    let blocks = parse(concat!(
        "<table><tbody align='right'>",
        "<tr><td>section</td><td style='text-align:center'>cell</td></tr>",
        "<tr align='left'><td>row</td><td align='right'>override</td></tr>",
        "</tbody></table>",
    ));
    let [Block::Table { rows, .. }] = blocks.as_slice() else {
        panic!("expected one table");
    };
    let alignment = |row: &[AlignedText]| row.iter().map(|cell| cell.alignment).collect::<Vec<_>>();
    assert_eq!(alignment(&rows[0]), [Alignment::Right, Alignment::Center]);
    assert_eq!(alignment(&rows[1]), [Alignment::Left, Alignment::Right]);
}

/// Preserve physical line associations for generated blocks, nested list items and raw HTML.
#[test]
fn markdown_source_rows_follow_visible_layout_blocks() {
    let markdown = super::parse(concat!(
        "# Top\n\n",
        "Written prose\n\n",
        "<p align=\"center\">\n",
        "<img src=\"logo.png\">\n",
        "</p>\n\n",
        "- first\n",
        "- second\n\n",
        "```plain\n",
        "first code line\n",
        "second code line\n",
        "```\n\n",
        "| Heading |\n",
        "|---------|\n",
        "| row one |\n",
        "| row two |\n",
    ));
    let source_rows = markdown
        .blocks
        .iter()
        .map(|block| block.source_row)
        .collect::<Vec<_>>();
    assert_eq!(
        source_rows,
        [Some(0), Some(2), Some(4), Some(8), Some(11), Some(16)]
    );
    let Block::List { items, .. } = &markdown.blocks[3].block else {
        panic!("expected a list");
    };
    assert_eq!(items[0][0].source_row, Some(8));
    assert_eq!(items[1][0].source_row, Some(9));
    let Block::Code { line_sources, .. } = &markdown.blocks[4].block else {
        panic!("expected fenced code");
    };
    assert_eq!(line_sources, &[Some(12), Some(13)]);
    let Block::Table { row_sources, .. } = &markdown.blocks[5].block else {
        panic!("expected a table");
    };
    assert_eq!(row_sources, &[Some(16), Some(18), Some(19)]);
    assert_eq!(markdown.source_rows, 21);
}
