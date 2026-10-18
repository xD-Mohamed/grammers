// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module contains several free-standing utility functions.

use crc32fast::Hasher;

/// Removes all single-line comments from the contents.
pub(crate) fn remove_tl_comments(contents: &str) -> String {
    let mut result = String::with_capacity(contents.len());
    for line in contents.split_inclusive('\n') {
        if let Some((before_comment, _)) = line.split_once("//") {
            result.push_str(before_comment);
            if line.ends_with('\n') {
                result.push('\n');
            }
        } else {
            result.push_str(line);
        }
    }

    result.shrink_to_fit();
    result
}

/// Infers the identifier for a definition.
pub(crate) fn infer_id(definition: &str) -> u32 {
    #[allow(clippy::collapsible_str_replace)]
    let mut representation = definition
        .replace(":bytes ", ": string")
        .replace("?bytes ", "? string")
        .replace('<', " ")
        .replace('>', "")
        .replace('{', "")
        .replace('}', "");

    // Remove r" \w+:flags\.\d+\?true"
    while let Some(pos) = representation.find("?true") {
        let space = representation[..pos].rfind(' ').unwrap_or(0);
        representation.replace_range(space..pos + "?true".len(), "");
    }

    let mut hasher = Hasher::new();
    hasher.update(representation.as_bytes());
    hasher.finalize() // crc32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_comments_noop() {
        let data = "hello\nworld";
        assert_eq!(remove_tl_comments(data), data);

        let data = " \nhello\nworld\n ";
        assert_eq!(remove_tl_comments(data), data);
    }

    #[test]
    fn remove_comments_leading() {
        let input = " // hello\n world ";
        let expected = " \n world ";
        assert_eq!(remove_tl_comments(input), expected);
    }

    #[test]
    fn remove_comments_trailing() {
        let input = " \nhello \n // world \n \n ";
        let expected = " \nhello \n \n \n ";
        assert_eq!(remove_tl_comments(input), expected);
    }

    #[test]
    fn remove_comments_many() {
        let input = "no\n//yes\nno\n//yes\nno\n";
        let expected = "no\n\nno\n\nno\n";
        assert_eq!(remove_tl_comments(input), expected);
    }

    #[test]
    fn remove_comments_unicode_comment() {
        assert_eq!(remove_tl_comments("// é\nx#1 = X;"), "\nx#1 = X;");
    }

    #[test]
    fn remove_comments_unicode_before_comment() {
        assert_eq!(remove_tl_comments("é🙂 // 注釈\nβ"), "é🙂 \nβ");
    }

    #[test]
    fn remove_comments_preserves_existing_crlf_behavior() {
        assert_eq!(remove_tl_comments("a\r\nb // c\r\nd\r\n"), "a\r\nb \nd\r\n");
    }

    #[test]
    fn remove_comments_without_final_newline() {
        assert_eq!(remove_tl_comments("a // comment"), "a ");
    }

    #[test]
    fn remove_comments_preserves_lone_slashes() {
        assert_eq!(remove_tl_comments("/\na / b\n/"), "/\na / b\n/");
    }

    #[test]
    fn check_infer_id() {
        // Note the type `bytes`
        let def = "rpc_answer_dropped msg_id:long seq_no:int bytes:int = RpcDropAnswer";
        assert_eq!(infer_id(def), 0xa43ad8b7);

        // Note the use of angle brackets
        let def = "msgs_ack msg_ids:Vector<long> = MsgsAck";
        assert_eq!(infer_id(def), 0x62d6b459);

        // Note the use of curly brackets
        let def = "invokeAfterMsg {X:Type} msg_id:long query:!X = X";
        assert_eq!(infer_id(def), 0xcb9f372d);

        // Note the use of `true` flags
        let def = "inputMessagesFilterPhoneCalls flags:# missed:flags.0?true = MessagesFilter";
        assert_eq!(infer_id(def), 0x80c99768);
    }
}
