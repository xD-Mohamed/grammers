// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::io;

use grammers_tl_gen::{Config, Outputs, generate_rust_code};
use grammers_tl_parser::parse_tl_file;
use grammers_tl_parser::tl::Definition;

const LAYER: i32 = 0;

fn get_definitions(contents: &str) -> Vec<Definition> {
    parse_tl_file(contents).map(|d| d.unwrap()).collect()
}

fn gen_rust_code(definitions: &[Definition]) -> io::Result<(String, String, String, String)> {
    let mut outputs = Outputs {
        common: Vec::new(),
        types: Vec::new(),
        functions: Vec::new(),
        enums: Vec::new(),
    };

    generate_rust_code(
        &mut outputs,
        definitions,
        LAYER,
        &Config {
            gen_name_for_id: false,
            deserializable_functions: true,
            impl_debug: true,
            impl_from_enum: true,
            impl_from_type: true,
            impl_serde: true,
        },
    )?;

    Ok((
        String::from_utf8(outputs.common).unwrap(),
        String::from_utf8(outputs.types).unwrap(),
        String::from_utf8(outputs.functions).unwrap(),
        String::from_utf8(outputs.enums).unwrap(),
    ))
}

#[test]
fn generic_functions_use_generic_parameters() -> io::Result<()> {
    let definitions = get_definitions(
        "
        ---functions---
        invokeWithLayer#da9b0d0d {X:Type} layer:int query:!X = X;
    ",
    );
    let (_, _, functions, _) = gen_rust_code(&definitions)?;
    eprintln!("{functions}");

    assert!(functions.contains("pub struct InvokeWithLayer<X>"));
    assert!(functions.contains("pub query: X,"));
    assert!(functions.contains("impl<X> crate::Identifiable for InvokeWithLayer<X>"));
    assert!(
        functions
            .contains("impl<X: crate::Serializable> crate::Serializable for InvokeWithLayer<X>")
    );
    assert!(
        functions.contains(
            "impl<X: crate::Deserializable> crate::Deserializable for InvokeWithLayer<X>"
        )
    );
    assert!(
        functions.contains("impl<X: crate::RemoteCall> crate::RemoteCall for InvokeWithLayer<X>")
    );
    assert!(functions.contains("type Return = X::Return;"));
    Ok(())
}

#[test]
fn recursive_types_direct_boxed() -> io::Result<()> {
    let definitions = get_definitions(
        "
        textBold#6724abc4 text:RichText = RichText;
    ",
    );
    let (_, _, _, enums) = gen_rust_code(&definitions)?;
    eprintln!("{enums}");
    assert!(enums.contains("TextBold(Box<crate::types::TextBold>)"));
    assert!(enums.contains("RichText::TextBold(Box::new("));
    assert!(enums.contains("Self::TextBold(Box::new("));
    Ok(())
}

#[test]
fn recursive_types_indirect_boxed() -> io::Result<()> {
    let definitions = get_definitions(
        "
        messageExtendedMedia#ee479c64 media:MessageMedia = MessageExtendedMedia;
        messageMediaInvoice#f6a548d3 flags:# extended_media:flags.4?MessageExtendedMedia = MessageMedia;
    ",
    );
    let (_, _, _, enums) = gen_rust_code(&definitions)?;
    eprintln!("{enums}");
    assert!(enums.contains("Media(Box<crate::types::MessageExtendedMedia>),"));
    assert!(enums.contains("Box::new(crate::types::MessageExtendedMedia::deserialize("));
    assert!(enums.contains("MessageExtendedMedia::Media(Box::new("));
    assert!(enums.contains("Invoice(Box<crate::types::MessageMediaInvoice>),"));
    assert!(enums.contains("Box::new(crate::types::MessageMediaInvoice::deserialize("));
    assert!(enums.contains("MessageMedia::Invoice(Box::new("));
    Ok(())
}

#[test]
fn recursive_types_indirect_no_hang() -> io::Result<()> {
    let definitions = get_definitions(
        "
        inputUserFromMessage#1da448e2 peer:InputPeer msg_id:int user_id:long = InputUser;
        inputPeerUserFromMessage#a87b0a1c peer:InputPeer msg_id:int user_id:long = InputPeer;
    ",
    );
    let _ = gen_rust_code(&definitions)?;
    Ok(())
}

#[test]
fn recursive_types_vec_indirect_not_boxed() -> io::Result<()> {
    let definitions = get_definitions(
        "
        jsonObjectValue#c0de1bd9 key:string value:JSONValue = JSONObjectValue;

        jsonArray#f7444763 value:Vector<JSONValue> = JSONValue;
        jsonObject#99c1d49d value:Vector<JSONObjectValue> = JSONValue;
    ",
    );
    let (_, _, _, enums) = gen_rust_code(&definitions)?;
    eprintln!("{enums}");
    assert!(enums.contains("JsonObjectValue(crate::types::JsonObjectValue)"));
    assert!(enums.contains("JsonArray(crate::types::JsonArray)"));
    assert!(enums.contains("JsonObject(crate::types::JsonObject)"));
    Ok(())
}

#[test]
fn generic_bytes_with_serde_bytes() -> io::Result<()> {
    let definitions = get_definitions(
        r#"
        chatPhotoEmpty#37c1011c = ChatPhoto;
        chatPhoto#1c6e1c11 flags:# has_video:flags.0?true photo_id:long stripped_thumb:flags.1?bytes dc_id:int = ChatPhoto;
        "#,
    );

    let (_, types, _, _) = gen_rust_code(&definitions)?;
    eprintln!("{types}");
    assert!(types.contains(r#"#[serde(with = "serde_bytes")]"#));
    assert!(types.contains("pub stripped_thumb: Option<Vec<u8>>,"));
    Ok(())
}

#[test]
fn namespaced_definitions_are_boxed_for_size() -> io::Result<()> {
    let definitions = get_definitions(
        "
        wallet.card#8ed043b1 address:string balance:long = wallet.Card;
        card#11111111 address:string = Card;
    ",
    );
    let (_, _, _, enums) = gen_rust_code(&definitions)?;
    eprintln!("{enums}");

    // Matched by full name: the same name outside its namespace stays inline.
    assert!(enums.contains("Card(Box<crate::types::wallet::Card>),"));
    assert!(enums.contains("Card(crate::types::Card),"));
    assert!(enums.contains("Self::Card(Box::new(crate::types::wallet::Card::deserialize(buf)?)),"));
    Ok(())
}

#[test]
fn fields_boxed_for_size_keep_their_variant_inline() -> io::Result<()> {
    let definitions = get_definitions(
        "
        starGift#313a9547 id:long = StarGift;
        wallet.nftItem#173dfaf3 flags:# name:flags.1?string gift:flags.9?StarGift = wallet.NftItem;
        nftItem#22222222 flags:# gift:flags.9?StarGift = NftItem;
        messageMediaEmpty#3ded6320 = MessageMedia;
        messageReplyHeader#ddb5ab75 flags:# reply_media:flags.8?MessageMedia = MessageReplyHeader;
        otherHeader#33333333 flags:# reply_media:flags.8?MessageMedia = OtherHeader;
    ",
    );
    let (_, types, _, enums) = gen_rust_code(&definitions)?;
    eprintln!("{types}");

    assert!(types.contains("pub gift: Option<Box<crate::enums::StarGift>>,"));
    assert!(types.contains("Some(Box::new(crate::enums::StarGift::deserialize(buf)?))"));
    // Other fields of that definition, and the same field elsewhere, are untouched.
    assert!(types.contains("pub name: Option<String>,"));
    assert!(types.contains("pub gift: Option<crate::enums::StarGift>,"));
    // The definition itself is not boxed: existing matches on the variant keep working.
    assert!(enums.contains("Item(crate::types::wallet::NftItem),"));
    assert!(types.contains("pub reply_media: Option<Box<crate::enums::MessageMedia>>,"));
    assert!(types.contains("Some(Box::new(crate::enums::MessageMedia::deserialize(buf)?))"));
    assert!(types.contains("pub reply_media: Option<crate::enums::MessageMedia>,"));
    assert!(enums.contains("Header(crate::types::MessageReplyHeader),"));
    Ok(())
}

#[test]
fn large_update_variants_are_boxed_with_owned_conversions() -> io::Result<()> {
    let variants = [
        ("updateSentWalletTransaction", "SentWalletTransaction"),
        ("updateServiceNotification", "ServiceNotification"),
        ("updateSentPhoneCode", "SentPhoneCode"),
        ("updateDraftMessage", "DraftMessage"),
        ("updateChannelParticipant", "ChannelParticipant"),
        ("updateNotifySettings", "NotifySettings"),
        ("updateBotPrecheckoutQuery", "BotPrecheckoutQuery"),
        ("updatePeerWallpaper", "PeerWallpaper"),
        ("updateTheme", "Theme"),
        ("updateChatParticipant", "ChatParticipant"),
    ];
    let mut schema = String::new();
    for (index, (definition, _)) in variants.iter().enumerate() {
        schema.push_str(&format!(
            "{definition}#{:08x} value:int = Update;\n",
            0x70000000u32 + index as u32
        ));
    }
    schema.push_str("updateChannel#635b4c09 channel_id:long = Update;\n");
    let definitions = get_definitions(&schema);
    let (_, types, _, enums) = gen_rust_code(&definitions)?;
    for (_, variant) in variants {
        let payload = format!("crate::types::Update{variant}");
        assert!(enums.contains(&format!("{variant}(Box<{payload}>),")));
        assert!(enums.contains(&format!(
            "Self::{variant}(Box::new({payload}::deserialize(buf)?)),"
        )));
        assert!(enums.contains(&format!("Update::{variant}(Box::new(x))")));
        assert!(types.contains(&format!("crate::enums::Update::{variant}(x) => Ok(*x),")));
    }
    assert!(enums.contains("Channel(crate::types::UpdateChannel),"));
    assert!(enums.contains("Update::Channel(x)"));
    assert!(types.contains("crate::enums::Update::Channel(x) => Ok(x),"));
    Ok(())
}

#[test]
fn large_media_leaves_are_boxed_and_wrappers_stay_inline() -> io::Result<()> {
    let definitions = get_definitions(
        "
        webPage#e89c45b2 id:long = WebPage;
        webPageEmpty#211a1788 id:long = WebPage;
        game#bdf9653b id:long = Game;
        messageMediaWebPage#ddf10c3b webpage:WebPage = MessageMedia;
        messageMediaGame#fdb19008 game:Game = MessageMedia;
        updateWebPage#7f891213 webpage:WebPage pts:int pts_count:int = Update;
        updateChannelWebPage#2f2ba99f channel_id:long webpage:WebPage pts:int pts_count:int = Update;
    ",
    );
    let (_, types, _, enums) = gen_rust_code(&definitions)?;
    assert!(enums.contains("Page(Box<crate::types::WebPage>),"));
    assert!(enums.contains("Game(Box<crate::types::Game>),"));
    assert!(enums.contains("Self::Page(Box::new(crate::types::WebPage::deserialize(buf)?)),"));
    assert!(enums.contains("Self::Game(Box::new(crate::types::Game::deserialize(buf)?)),"));
    assert!(enums.contains("WebPage::Page(Box::new(x))"));
    assert!(enums.contains("Game::Game(Box::new(x))"));
    assert!(types.contains("crate::enums::WebPage::Page(x) => Ok(*x),"));
    assert!(types.contains("crate::enums::Game::Game(x) => *x,"));
    assert!(enums.contains("Empty(crate::types::WebPageEmpty),"));
    for (variant, payload) in [
        ("WebPage", "MessageMediaWebPage"),
        ("Game", "MessageMediaGame"),
        ("WebPage", "UpdateWebPage"),
        ("ChannelWebPage", "UpdateChannelWebPage"),
    ] {
        assert!(enums.contains(&format!("{variant}(crate::types::{payload}),")));
    }
    assert!(enums.contains("Update::WebPage(x)"));
    assert!(enums.contains("Update::ChannelWebPage(x)"));
    assert!(types.contains("crate::enums::Update::WebPage(x) => Ok(x),"));
    assert!(types.contains("crate::enums::Update::ChannelWebPage(x) => Ok(x),"));
    Ok(())
}

#[test]
fn input_rich_message_leaves_are_boxed_with_owned_conversions() -> io::Result<()> {
    let definitions = get_definitions(
        "
        inputRichMessage#e4c449fc flags:# rtl:flags.0?true blocks:Vector<int> photos:flags.2?Vector<int> = InputRichMessage;
        inputRichMessageHTML#dacb836a flags:# rtl:flags.0?true html:string = InputRichMessage;
        inputRichMessageMarkdown#004b572c flags:# rtl:flags.0?true markdown:string = InputRichMessage;
        sample.inputRichMessage#70000001 blocks:Vector<int> = sample.InputRichMessage;
    ",
    );
    let (_, types, _, enums) = gen_rust_code(&definitions)?;
    for (variant, payload) in [
        ("Message", "InputRichMessage"),
        ("Html", "InputRichMessageHtml"),
        ("Markdown", "InputRichMessageMarkdown"),
    ] {
        assert!(enums.contains(&format!("{variant}(Box<crate::types::{payload}>),")));
        assert!(enums.contains(&format!(
            "Self::{variant}(Box::new(crate::types::{payload}::deserialize(buf)?)),"
        )));
        assert!(enums.contains(&format!("InputRichMessage::{variant}(Box::new(x))")));
        assert!(types.contains(&format!(
            "crate::enums::InputRichMessage::{variant}(x) => Ok(*x),"
        )));
    }
    assert!(types.contains("pub blocks: Vec<i32>,"));
    assert!(types.contains("pub photos: Option<Vec<i32>>,"));
    assert!(enums.contains("Message(crate::types::sample::InputRichMessage),"));
    Ok(())
}

#[test]
fn collectible_leaves_are_boxed_and_ordinary_ones_stay_inline() -> io::Result<()> {
    let definitions = get_definitions(
        "
        emojiStatus#e7ff068a flags:# document_id:long until:flags.0?int = EmojiStatus;
        emojiStatusCollectible#7184603b flags:# collectible_id:long title:string until:flags.0?int = EmojiStatus;
        peerColor#b54b5acf flags:# color:flags.0?int = PeerColor;
        peerColorCollectible#b9c0639a flags:# collectible_id:long colors:Vector<int> = PeerColor;
        sample.peerColorCollectible#70000002 colors:Vector<int> = sample.PeerColor;
    ",
    );
    let (_, types, _, enums) = gen_rust_code(&definitions)?;
    for (ty, payload) in [
        ("EmojiStatus", "EmojiStatusCollectible"),
        ("PeerColor", "PeerColorCollectible"),
    ] {
        assert!(enums.contains(&format!("Collectible(Box<crate::types::{payload}>),")));
        assert!(enums.contains(&format!(
            "Self::Collectible(Box::new(crate::types::{payload}::deserialize(buf)?)),"
        )));
        assert!(enums.contains(&format!("{ty}::Collectible(Box::new(x))")));
        assert!(types.contains(&format!("crate::enums::{ty}::Collectible(x) => Ok(*x),")));
    }
    // What nearly every user and channel has stays inline, and so does a namesake
    // in another namespace.
    assert!(enums.contains("Status(crate::types::EmojiStatus),"));
    assert!(enums.contains("Color(crate::types::PeerColor),"));
    assert!(enums.contains("Collectible(crate::types::sample::PeerColorCollectible),"));
    Ok(())
}

#[test]
fn short_update_fields_are_boxed_only_in_those_updates() -> io::Result<()> {
    let definitions = get_definitions(
        "
        messageMediaEmpty#3ded6320 = MessageMedia;
        messageFwdHeader#4e4df4bb date:int = MessageFwdHeader;
        messageReplyHeader#ddb5ab75 flags:# reply_to_msg_id:flags.4?int reply_media:flags.8?MessageMedia = MessageReplyHeader;
        updateShortMessage#313bc7f8 flags:# id:int fwd_from:flags.2?MessageFwdHeader reply_to:flags.3?MessageReplyHeader = Updates;
        updateShortChatMessage#4d6deea5 flags:# id:int fwd_from:flags.2?MessageFwdHeader reply_to:flags.3?MessageReplyHeader = Updates;
        updateShortSentMessage#9015e101 flags:# id:int media:flags.9?MessageMedia = Updates;
        message#9d2e9f66 flags:# id:int fwd_from:flags.2?MessageFwdHeader reply_to:flags.3?MessageReplyHeader media:flags.9?MessageMedia = Message;
    ",
    );
    let (_, types, _, enums) = gen_rust_code(&definitions)?;
    let count = |needle: &str| types.matches(needle).count();
    // The reply header's own boxed media is the second boxed `MessageMedia` decoder.
    for (field, ty, boxed, decoders) in [
        ("fwd_from", "MessageFwdHeader", 2, 2),
        ("reply_to", "MessageReplyHeader", 2, 2),
        ("media", "MessageMedia", 1, 2),
    ] {
        assert_eq!(
            count(&format!("pub {field}: Option<Box<crate::enums::{ty}>>,")),
            boxed
        );
        assert_eq!(
            count(&format!(
                "Some(Box::new(crate::enums::{ty}::deserialize(buf)?))"
            )),
            decoders
        );
        // A message keeps all three inline: it is where these fields end up.
        assert_eq!(
            count(&format!("pub {field}: Option<crate::enums::{ty}>,")),
            1
        );
    }
    // The updates themselves stay inline variants, matched as before.
    assert!(enums.contains("UpdateShortMessage(crate::types::UpdateShortMessage),"));
    assert!(enums.contains("UpdateShortChatMessage(crate::types::UpdateShortChatMessage),"));
    assert!(enums.contains("UpdateShortSentMessage(crate::types::UpdateShortSentMessage),"));
    Ok(())
}
