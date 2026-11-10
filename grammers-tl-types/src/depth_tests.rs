// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.

use super::{Cursor, Error, MAX_DEPTH};
use crate::{Deserializable, Identifiable, Serializable, enums, types};

const LIMIT: usize = MAX_DEPTH as usize;

fn id<T: Identifiable>(bytes: &mut Vec<u8>) {
    T::CONSTRUCTOR_ID.serialize(bytes);
}

fn one_item_vector(bytes: &mut Vec<u8>) {
    0x1cb5c415u32.serialize(bytes);
    1u32.serialize(bytes);
}

fn empty_vector(bytes: &mut Vec<u8>) {
    0x1cb5c415u32.serialize(bytes);
    0u32.serialize(bytes);
}

// `wrappers` TextBold values around one TextPlain: wrappers + 1 boxed levels.
fn rich_text(wrappers: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..wrappers {
        id::<types::TextBold>(&mut bytes);
    }
    id::<types::TextPlain>(&mut bytes);
    b"owned".as_slice().serialize(&mut bytes);
    bytes
}

fn json_array(wrappers: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..wrappers {
        id::<types::JsonArray>(&mut bytes);
        one_item_vector(&mut bytes);
    }
    id::<types::JsonString>(&mut bytes);
    b"owned".as_slice().serialize(&mut bytes);
    bytes
}

// Each repetition adds two boxed levels: MessageMedia and StoryItem.
fn story_media(repetitions: usize, missing_albums: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for level in 0..repetitions {
        id::<types::MessageMediaStory>(&mut bytes);
        1u32.serialize(&mut bytes);
        id::<types::PeerUser>(&mut bytes);
        1i64.serialize(&mut bytes);
        1i32.serialize(&mut bytes);
        id::<types::StoryItem>(&mut bytes);
        (if level == 0 && missing_albums {
            1u32 << 19
        } else {
            0
        })
        .serialize(&mut bytes);
        1i32.serialize(&mut bytes);
        1i32.serialize(&mut bytes);
        2i32.serialize(&mut bytes);
    }
    id::<types::MessageMediaEmpty>(&mut bytes);
    bytes
}

// The largest measured decoder frames: PageBlockDetails inside an Instant View.
fn nested_details(wrappers: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..wrappers {
        id::<types::PageBlockDetails>(&mut bytes);
        0u32.serialize(&mut bytes);
        one_item_vector(&mut bytes);
    }
    id::<types::PageBlockDivider>(&mut bytes);
    for _ in 0..wrappers {
        bytes.extend(rich_text(0));
    }
    bytes
}

fn web_page_media(blocks: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::MessageMediaWebPage>(&mut bytes);
    0u32.serialize(&mut bytes);
    id::<types::WebPage>(&mut bytes);
    (1u32 << 10).serialize(&mut bytes);
    1i64.serialize(&mut bytes);
    b"url".as_slice().serialize(&mut bytes);
    b"url".as_slice().serialize(&mut bytes);
    0i32.serialize(&mut bytes);
    id::<types::Page>(&mut bytes);
    0u32.serialize(&mut bytes);
    b"url".as_slice().serialize(&mut bytes);
    one_item_vector(&mut bytes);
    bytes.extend(blocks);
    empty_vector(&mut bytes);
    empty_vector(&mut bytes);
    bytes
}

// Updates, Update and Message contribute three boxed levels before the media.
fn message_update(media: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::UpdateShort>(&mut bytes);
    id::<types::UpdateNewMessage>(&mut bytes);
    id::<types::Message>(&mut bytes);
    (1u32 << 9).serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    1i32.serialize(&mut bytes);
    id::<types::PeerUser>(&mut bytes);
    1i64.serialize(&mut bytes);
    1i32.serialize(&mut bytes);
    b"owned".as_slice().serialize(&mut bytes);
    bytes.extend(media);
    1i32.serialize(&mut bytes);
    1i32.serialize(&mut bytes);
    1i32.serialize(&mut bytes);
    bytes
}

// A realistic Instant View: the path that an overly small limit would reject.
fn realistic_instant_view() -> Vec<u8> {
    let mut paragraph = Vec::new();
    id::<types::PageBlockParagraph>(&mut paragraph);
    paragraph.extend(rich_text(8));
    let mut item = Vec::new();
    id::<types::PageListItemBlocks>(&mut item);
    0u32.serialize(&mut item);
    one_item_vector(&mut item);
    item.extend(paragraph);
    let mut list = Vec::new();
    id::<types::PageBlockList>(&mut list);
    one_item_vector(&mut list);
    list.extend(item);
    let mut details = Vec::new();
    id::<types::PageBlockDetails>(&mut details);
    0u32.serialize(&mut details);
    one_item_vector(&mut details);
    details.extend(list);
    details.extend(rich_text(0));
    message_update(web_page_media(details))
}

// `wallet.nftItem`, whose optional `gift` is held in a `Box` for size.
fn nft_item(with_gift: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::wallet::NftItem>(&mut bytes);
    (if with_gift { 1u32 << 9 } else { 0 }).serialize(&mut bytes);
    b"address".as_slice().serialize(&mut bytes);
    b"owner".as_slice().serialize(&mut bytes);
    b"1".as_slice().serialize(&mut bytes);
    if with_gift {
        id::<types::StarGift>(&mut bytes);
        0u32.serialize(&mut bytes);
        7i64.serialize(&mut bytes);
        id::<types::DocumentEmpty>(&mut bytes);
        2i64.serialize(&mut bytes);
        3i64.serialize(&mut bytes);
        4i64.serialize(&mut bytes);
    }
    bytes
}

// `wallet.card`, boxed for size, holding one item as its background.
fn wallet_card(item: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::wallet::Card>(&mut bytes);
    (1u32 << 1).serialize(&mut bytes);
    bytes.extend(item);
    b"address".as_slice().serialize(&mut bytes);
    5i64.serialize(&mut bytes);
    bytes
}

fn round_trip<T: Deserializable + Serializable + Clone + std::fmt::Debug>(bytes: &[u8]) {
    let mut cursor = Cursor::from_slice(bytes);
    let value = T::deserialize(&mut cursor).unwrap();
    assert_eq!(cursor.pos(), bytes.len());
    assert_eq!(value.to_bytes(), bytes);
    // Ordinary recursive operations on a decoded value stay bounded too.
    let copy = value.clone();
    assert!(!format!("{copy:?}").is_empty());
    drop(copy);
    drop(value);
}

fn assert_error<T: Deserializable>(bytes: &[u8], expected: Error) {
    assert_eq!(T::from_bytes(bytes).err(), Some(expected));
}

fn generated_cases() {
    // Exact bound for Box-recursive and vector-recursive values.
    round_trip::<enums::RichText>(&rich_text(LIMIT - 1));
    round_trip::<enums::Jsonvalue>(&json_array(LIMIT - 1));
    for wrappers in [LIMIT, 10_000] {
        assert_error::<enums::RichText>(&rich_text(wrappers), Error::DepthLimitExceeded);
        assert_error::<enums::Jsonvalue>(&json_array(wrappers), Error::DepthLimitExceeded);
    }
    // Breadth reuses depth; failure drops previously completed owned subtrees.
    let mut wide = Vec::new();
    id::<types::TextConcat>(&mut wide);
    0x1cb5c415u32.serialize(&mut wide);
    256u32.serialize(&mut wide);
    for _ in 0..256 {
        wide.extend(rich_text(LIMIT - 2));
    }
    round_trip::<enums::RichText>(&wide);
    let mut partial = wide.clone();
    partial[8..12].copy_from_slice(&257u32.to_le_bytes());
    assert_error::<enums::RichText>(&partial, Error::UnexpectedEof);
    let mut malformed = partial.clone();
    malformed.extend(0xdeadbeefu32.to_le_bytes());
    assert_error::<enums::RichText>(&malformed, Error::UnexpectedConstructor { id: 0xdeadbeef });
    partial.extend(rich_text(LIMIT - 1));
    assert_error::<enums::RichText>(&partial, Error::DepthLimitExceeded);
    let mut late_field = Vec::new();
    id::<types::TextUrl>(&mut late_field);
    late_field.extend(rich_text(LIMIT - 2));
    assert_error::<enums::RichText>(&late_field, Error::UnexpectedEof);
    // The recursive MessageMedia/StoryItem decoder and drop frames.
    let repetitions = (LIMIT - 1) / 2;
    round_trip::<enums::MessageMedia>(&story_media(repetitions, false));
    assert_error::<enums::MessageMedia>(
        &story_media(repetitions + 1, false),
        Error::DepthLimitExceeded,
    );
    assert_error::<enums::MessageMedia>(&story_media(10_000, false), Error::DepthLimitExceeded);
    assert_error::<enums::MessageMedia>(&story_media(repetitions, true), Error::UnexpectedEof);
    // Worst measured stack: complete Updates prefix plus nested PageBlockDetails.
    // Updates, Update, Message, MessageMedia, WebPage and Page precede the blocks.
    let wrappers = LIMIT - 7;
    let deepest = message_update(web_page_media(nested_details(wrappers)));
    round_trip::<enums::Updates>(&deepest);
    let mut incomplete = deepest.clone();
    incomplete.truncate(incomplete.len() - 4);
    assert_error::<enums::Updates>(&incomplete, Error::UnexpectedEof);
    assert_error::<enums::Updates>(
        &message_update(web_page_media(nested_details(wrappers + 1))),
        Error::DepthLimitExceeded,
    );
    round_trip::<enums::Updates>(&realistic_instant_view());
    // Errors restore the depth budget but do not rewind consumed wire bytes.
    let rejected = rich_text(10_000);
    let mut bytes = rejected.clone();
    bytes.extend(rich_text(LIMIT - 1));
    let mut cursor = Cursor::from_slice(&bytes);
    assert_eq!(
        enums::RichText::deserialize(&mut cursor).err(),
        Some(Error::DepthLimitExceeded)
    );
    cursor.read_slice(rejected.len() - cursor.pos()).unwrap();
    drop(enums::RichText::deserialize(&mut cursor).unwrap());
    assert_eq!(cursor.pos(), bytes.len());
}

#[test]
fn generated_values_are_bounded_on_standard_stack() {
    const CHILD: &str = "GRAMMERS_TL_DEPTH_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(generated_cases)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    // Stack overflow aborts instead of unwinding, so check a real child exit.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "deserialize::depth_tests::generated_values_are_bounded_on_standard_stack",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
fn depth_scope_restores_after_unwind() {
    let mut cursor = Cursor::from_slice(&[]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut outer = cursor.enter().unwrap();
        let _inner = outer.cursor().enter().unwrap();
        panic!("exercise guard cleanup");
    }));
    assert!(result.is_err());
    assert_eq!(cursor.depth, 0);
}

// Boxing for size changes where a value lives, never its bytes: with and without the
// boxed field, inside the boxed definition, and when the input ends inside either.
#[test]
fn values_boxed_for_size_keep_their_wire_format() {
    round_trip::<enums::wallet::NftItem>(&nft_item(false));
    round_trip::<enums::wallet::NftItem>(&nft_item(true));
    let enums::wallet::NftItem::Item(item) =
        enums::wallet::NftItem::from_bytes(&nft_item(true)).unwrap();
    assert!(matches!(item.gift.as_deref(), Some(enums::StarGift::Gift(gift)) if gift.id == 7));
    let enums::wallet::NftItem::Item(item) =
        enums::wallet::NftItem::from_bytes(&nft_item(false)).unwrap();
    assert!(item.gift.is_none());

    let card = wallet_card(nft_item(true));
    round_trip::<enums::wallet::Card>(&card);
    let enums::wallet::Card::Card(decoded) = enums::wallet::Card::from_bytes(&card).unwrap();
    assert_eq!(decoded.balance, 5);
    assert!(decoded.bg_nft_item.is_some());
    // A value built by hand takes the same form on the wire as a decoded one.
    let rebuilt: enums::wallet::Card = (*decoded).clone().into();
    assert_eq!(rebuilt.to_bytes(), card);
    for end in [card.len() - 4, card.len() - 20, 40, 12] {
        assert_error::<enums::wallet::Card>(&card[..end], Error::UnexpectedEof);
    }
}

fn reply_header(with_media: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::MessageReplyHeader>(&mut bytes);
    let flags = (1u32 << 4) | (1 << 6) | if with_media { 1 << 8 } else { 0 };
    flags.serialize(&mut bytes);
    41i32.serialize(&mut bytes);
    if with_media {
        id::<types::MessageMediaContact>(&mut bytes);
        b"123".as_slice().serialize(&mut bytes);
        b"first".as_slice().serialize(&mut bytes);
        b"last".as_slice().serialize(&mut bytes);
        b"card".as_slice().serialize(&mut bytes);
        7i64.serialize(&mut bytes);
    }
    b"quoted text".as_slice().serialize(&mut bytes);
    bytes
}

#[test]
fn boxed_reply_media_keeps_its_wire_format() {
    for with_media in [false, true] {
        let bytes = reply_header(with_media);
        round_trip::<enums::MessageReplyHeader>(&bytes);
        let enums::MessageReplyHeader::Header(header) =
            enums::MessageReplyHeader::from_bytes(&bytes).unwrap()
        else {
            panic!("expected a message reply header");
        };
        assert_eq!(header.reply_to_msg_id, Some(41));
        assert_eq!(header.quote_text.as_deref(), Some("quoted text"));
        if with_media {
            assert!(matches!(
                header.reply_media.as_deref(),
                Some(enums::MessageMedia::Contact(contact)) if contact.user_id == 7
            ));
        } else {
            assert!(header.reply_media.is_none());
        }
        let rebuilt: enums::MessageReplyHeader = header.into();
        assert_eq!(rebuilt.to_bytes(), bytes);
        // Includes failure after media has allocated its strings, while reading
        // the following quote: partially decoded fields must drop safely.
        for end in 0..bytes.len() {
            assert_error::<enums::MessageReplyHeader>(&bytes[..end], Error::UnexpectedEof);
        }
    }
}

fn service_notification(with_inbox_date: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::UpdateServiceNotification>(&mut bytes);
    let flags = 1u32 | (1 << 2) | if with_inbox_date { 1 << 1 } else { 0 };
    flags.serialize(&mut bytes);
    if with_inbox_date {
        123i32.serialize(&mut bytes);
    }
    b"service".as_slice().serialize(&mut bytes);
    b"owned message".as_slice().serialize(&mut bytes);
    id::<types::MessageMediaContact>(&mut bytes);
    b"123".as_slice().serialize(&mut bytes);
    b"first".as_slice().serialize(&mut bytes);
    b"last".as_slice().serialize(&mut bytes);
    b"card".as_slice().serialize(&mut bytes);
    7i64.serialize(&mut bytes);
    empty_vector(&mut bytes);
    bytes
}

#[test]
fn boxed_updates_keep_wire_format_and_owned_conversions() {
    for with_inbox_date in [false, true] {
        let bytes = service_notification(with_inbox_date);
        round_trip::<enums::Update>(&bytes);
        let update = enums::Update::from_bytes(&bytes).unwrap();
        let enums::Update::ServiceNotification(notification) = &update else {
            panic!("expected a service notification");
        };
        assert!(notification.popup);
        assert!(notification.invert_media);
        assert_eq!(notification.inbox_date, with_inbox_date.then_some(123));
        assert_eq!(notification.r#type, "service");
        assert_eq!(notification.message, "owned message");
        assert!(notification.entities.is_empty());
        assert!(matches!(
            &notification.media,
            enums::MessageMedia::Contact(contact) if contact.user_id == 7
        ));
        let owned = types::UpdateServiceNotification::try_from(update).unwrap();
        let rebuilt: enums::Update = owned.into();
        assert_eq!(rebuilt.to_bytes(), bytes);
        for end in 0..bytes.len() {
            assert_error::<enums::Update>(&bytes[..end], Error::UnexpectedEof);
        }
    }
    let mut bytes = Vec::new();
    id::<types::UpdateChannel>(&mut bytes);
    7i64.serialize(&mut bytes);
    round_trip::<enums::Update>(&bytes);
    let enums::Update::Channel(channel) = enums::Update::from_bytes(&bytes).unwrap() else {
        panic!("expected a channel update");
    };
    let channel: types::UpdateChannel = channel;
    assert_eq!(channel.channel_id, 7);
}

fn small_web_page() -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::WebPage>(&mut bytes);
    0u32.serialize(&mut bytes);
    7i64.serialize(&mut bytes);
    b"https://example.test/".as_slice().serialize(&mut bytes);
    b"example.test".as_slice().serialize(&mut bytes);
    17i32.serialize(&mut bytes);
    bytes
}

fn small_game() -> Vec<u8> {
    let mut bytes = Vec::new();
    id::<types::Game>(&mut bytes);
    0u32.serialize(&mut bytes);
    7i64.serialize(&mut bytes);
    9i64.serialize(&mut bytes);
    b"short".as_slice().serialize(&mut bytes);
    b"title".as_slice().serialize(&mut bytes);
    b"description".as_slice().serialize(&mut bytes);
    id::<types::PhotoEmpty>(&mut bytes);
    23i64.serialize(&mut bytes);
    bytes
}

#[test]
fn boxed_media_leaves_keep_their_wire_format() {
    let page_bytes = small_web_page();
    round_trip::<enums::WebPage>(&page_bytes);
    let page = enums::WebPage::from_bytes(&page_bytes).unwrap();
    let enums::WebPage::Page(raw) = &page else {
        panic!("expected a full webpage");
    };
    assert_eq!(raw.id, 7);
    assert_eq!(raw.url, "https://example.test/");
    assert_eq!(raw.display_url, "example.test");
    assert_eq!(raw.hash, 17);
    assert!(raw.photo.is_none());
    let rebuilt: enums::WebPage = types::WebPage::try_from(page).unwrap().into();
    assert_eq!(rebuilt.to_bytes(), page_bytes);
    for end in 0..page_bytes.len() {
        assert_error::<enums::WebPage>(&page_bytes[..end], Error::UnexpectedEof);
    }

    let game_bytes = small_game();
    round_trip::<enums::Game>(&game_bytes);
    let game = enums::Game::from_bytes(&game_bytes).unwrap();
    let enums::Game::Game(raw) = &game;
    assert_eq!(raw.id, 7);
    assert_eq!(raw.access_hash, 9);
    assert_eq!(raw.short_name, "short");
    assert_eq!(raw.title, "title");
    assert_eq!(raw.description, "description");
    assert!(matches!(&raw.photo, enums::Photo::Empty(photo) if photo.id == 23));
    assert!(raw.document.is_none());
    let rebuilt: enums::Game = types::Game::from(game).into();
    assert_eq!(rebuilt.to_bytes(), game_bytes);
    for end in 0..game_bytes.len() {
        assert_error::<enums::Game>(&game_bytes[..end], Error::UnexpectedEof);
    }

    let mut page_media = Vec::new();
    id::<types::MessageMediaWebPage>(&mut page_media);
    0u32.serialize(&mut page_media);
    page_media.extend_from_slice(&page_bytes);
    round_trip::<enums::MessageMedia>(&page_media);
    let enums::MessageMedia::WebPage(raw) = enums::MessageMedia::from_bytes(&page_media).unwrap()
    else {
        panic!("expected webpage media");
    };
    let _: types::MessageMediaWebPage = raw;

    let mut game_media = Vec::new();
    id::<types::MessageMediaGame>(&mut game_media);
    game_media.extend_from_slice(&game_bytes);
    round_trip::<enums::MessageMedia>(&game_media);
    let enums::MessageMedia::Game(raw) = enums::MessageMedia::from_bytes(&game_media).unwrap()
    else {
        panic!("expected game media");
    };
    let _: types::MessageMediaGame = raw;
    for bytes in [&page_media, &game_media] {
        for end in 0..bytes.len() {
            assert_error::<enums::MessageMedia>(&bytes[..end], Error::UnexpectedEof);
        }
    }
}

// These values move through every step of the update path. Holding the largest variants
// inline made a single update over 5 KiB, so a schema change undoing this must be noticed.
// At layer 233 `Update` measured 1,264 B with `wallet.card` and `wallet.nftItem`'s `gift`
// inline, and measured 760 B with the first boxed and the second a boxed field,
// before the rare update variants were boxed too.
#[test]
fn update_types_stay_small_enough_to_move_around() {
    eprintln!(
        "TL sizes: MessageReplyHeader={} Message={} MessageService={} Update={} Updates={} Media={} WebPage={} Game={}",
        size_of::<types::MessageReplyHeader>(),
        size_of::<types::Message>(),
        size_of::<types::MessageService>(),
        size_of::<enums::Update>(),
        size_of::<enums::Updates>(),
        size_of::<enums::MessageMedia>(),
        size_of::<enums::WebPage>(),
        size_of::<enums::Game>(),
    );
    assert!(size_of::<enums::Message>() <= 32);
    assert!(size_of::<enums::EphemeralMessage>() <= 32);
    assert!(size_of::<enums::wallet::Card>() <= 8);
    assert!(
        size_of::<enums::WebPage>() <= 64,
        "{}",
        size_of::<enums::WebPage>()
    );
    assert!(size_of::<enums::Game>() <= 8);
    assert!(
        size_of::<enums::MessageMedia>() <= 320,
        "{}",
        size_of::<enums::MessageMedia>()
    );
    assert!(
        size_of::<types::MessageReplyHeader>() <= 384,
        "{}",
        size_of::<types::MessageReplyHeader>()
    );
    assert!(
        size_of::<types::Message>() <= 1664,
        "{}",
        size_of::<types::Message>()
    );
    assert!(
        size_of::<types::UpdateSentWalletTransaction>() <= 768,
        "{}",
        size_of::<types::UpdateSentWalletTransaction>()
    );
    assert!(
        size_of::<enums::Update>() <= 256,
        "{}",
        size_of::<enums::Update>()
    );
    assert!(
        size_of::<enums::Updates>() <= 1024,
        "{}",
        size_of::<enums::Updates>()
    );
}

fn input_rich_bytes(kind: u8, with_collections: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    match kind {
        0 => id::<types::InputRichMessage>(&mut bytes),
        1 => id::<types::InputRichMessageHtml>(&mut bytes),
        2 => id::<types::InputRichMessageMarkdown>(&mut bytes),
        _ => panic!("unknown rich-message fixture"),
    }
    let flags = 3u32
        | if with_collections {
            if kind == 0 { 0x1c } else { 4 }
        } else {
            0
        };
    flags.serialize(&mut bytes);
    match kind {
        0 => {
            one_item_vector(&mut bytes);
            id::<types::PageBlockParagraph>(&mut bytes);
            bytes.extend_from_slice(&rich_text(0));
            if with_collections {
                empty_vector(&mut bytes);
                empty_vector(&mut bytes);
                empty_vector(&mut bytes);
            }
        }
        1 | 2 => {
            if kind == 1 {
                b"<b>owned</b>".as_slice().serialize(&mut bytes);
            } else {
                b"**owned**".as_slice().serialize(&mut bytes);
            }
            if with_collections {
                empty_vector(&mut bytes);
            }
        }
        _ => unreachable!(),
    }
    bytes
}

#[test]
fn boxed_input_rich_messages_keep_their_wire_format() {
    for kind in 0..=2 {
        for with_collections in [false, true] {
            let bytes = input_rich_bytes(kind, with_collections);
            round_trip::<enums::InputRichMessage>(&bytes);
            let value = enums::InputRichMessage::from_bytes(&bytes).unwrap();
            match &value {
                enums::InputRichMessage::Message(message) => {
                    assert_eq!(kind, 0);
                    assert!(message.rtl && message.noautolink);
                    assert_eq!(message.blocks.len(), 1);
                    assert_eq!(message.photos.is_some(), with_collections);
                    assert_eq!(message.documents.is_some(), with_collections);
                    assert_eq!(message.users.is_some(), with_collections);
                }
                enums::InputRichMessage::Html(message) => {
                    assert_eq!(kind, 1);
                    assert!(message.rtl && message.noautolink);
                    assert_eq!(message.html, "<b>owned</b>");
                    assert_eq!(message.files.is_some(), with_collections);
                }
                enums::InputRichMessage::Markdown(message) => {
                    assert_eq!(kind, 2);
                    assert!(message.rtl && message.noautolink);
                    assert_eq!(message.markdown, "**owned**");
                    assert_eq!(message.files.is_some(), with_collections);
                }
            }
            let rebuilt: enums::InputRichMessage = match kind {
                0 => types::InputRichMessage::try_from(value).unwrap().into(),
                1 => types::InputRichMessageHtml::try_from(value).unwrap().into(),
                2 => types::InputRichMessageMarkdown::try_from(value)
                    .unwrap()
                    .into(),
                _ => unreachable!(),
            };
            assert_eq!(rebuilt.to_bytes(), bytes);
            for end in 0..bytes.len() {
                assert_error::<enums::InputRichMessage>(&bytes[..end], Error::UnexpectedEof);
            }
        }
    }
    eprintln!(
        "InputRichMessage={} SendMessage={}",
        size_of::<enums::InputRichMessage>(),
        size_of::<crate::functions::messages::SendMessage>()
    );
    assert!(size_of::<enums::InputRichMessage>() <= 16);
    assert!(size_of::<crate::functions::messages::SendMessage>() <= 512);
}
