use super::*;

const DOCUMENT: &str = r#"<?xml version="1.0"?>
<session-journal formatVersion="1" id="replace" xmlns:ext="urn:extension">
  <meta><title>secret metadata</title><description>secret metadata</description></meta>
  <entries>
    <entry secret="attribute">
      <title>secret secret</title>
      <text data="x>y"><![CDATA[se]]><!--keep-->cr<?keep this?>et</text>
      <userNote>secret</userNote><aiDescription>secret</aiDescription>
      <inputExcerpt><line>secret</line></inputExcerpt>
      <outputExcerpt><line>secret</line></outputExcerpt>
      <aiTags><tag>secret</tag></aiTags>
      <chatSnapshot><title>secret</title><frozen>true</frozen><messages>
        <message><role>secret role</role><content>secret</content><aiProfileName>secret</aiProfileName>
          <steps><step><title>secret</title><summary>secret</summary><command>secret</command>
            <output>secret</output><status>secret status</status><exitCode>0</exitCode></step></steps>
        </message></messages></chatSnapshot>
      <ext:text>secret extension</ext:text><extension><title>secret extension</title></extension>
      <screenshotFile>screenshots/secret.png</screenshotFile>
    </entry>
  </entries>
</session-journal>
"#;

#[test]
fn journal_replace_xml_rewrites_only_pinned_entry_and_chat_text() {
    let rule = Rule::compile("secret", "<x&>\r", false, false).unwrap();
    let (count, result) = rewrite(DOCUMENT, &rule).unwrap();
    assert_eq!(count, 14);
    let result = result.unwrap();
    let expected = DOCUMENT
        .replace(">secret<", ">&lt;x&amp;&gt;&#13;<")
        .replace(
            ">secret secret<",
            ">&lt;x&amp;&gt;&#13; &lt;x&amp;&gt;&#13;<",
        )
        .replace(
            "<![CDATA[se]]><!--keep-->cr<?keep this?>et",
            "&lt;x&amp;&gt;&#13;<!--keep--><?keep this?>",
        );
    assert_eq!(result.as_str(), expected);
    let (count, unchanged) = rewrite(&result, &rule).unwrap();
    assert_eq!(count, 0);
    assert!(unchanged.is_none());
}

#[test]
fn journal_replace_xml_handles_empty_result_and_rejects_invalid_xml_characters() {
    let input = "<session-journal formatVersion=\"1\" id=\"replace\"><meta/>\
                 <entries><entry><title>&amp; &#13;</title><text/></entry></entries></session-journal>";
    let rule = Rule::compile("& \r", "", false, false).unwrap();
    let (count, result) = rewrite(input, &rule).unwrap();
    assert_eq!(count, 1);
    assert_eq!(result.unwrap().as_str(), input.replace("&amp; &#13;", ""));

    let forbidden = Rule::compile("&", "\0", false, false).unwrap();
    assert!(rewrite(input, &forbidden).is_err());
    let nested = input.replace("&amp; &#13;", "<unsupported/>");
    assert!(rewrite(&nested, &rule).is_err());
}
