//! The read-only database layer, driven against a synthetic fixture.
//!
//! The fixture is built by `tests/fixtures/mod.rs` and carries the real macOS
//! column names. Nothing here opens `~/Library/Messages/chat.db`, and nothing
//! here writes to any database — the last test proves the second half of that.

mod fixtures;

use msgs::app::{Action, App, DbStatus};
use msgs::config::Config;
use msgs::db::{AttachmentKind, Db, GroupAction, Source, TapbackKind};
use msgs::send::{Delivery, Pending};

fn db() -> Db {
    Db::open(&fixtures::database()).expect("open the fixture read-only")
}

#[test]
fn the_fixture_opens_read_only_and_in_place() {
    let db = db();
    assert_eq!(db.source(), Source::Live);
    assert!(db.scratch_path().is_none());

    let counts = db.counts().expect("counts");
    // Four `chat` rows, but three conversations: two of the rows are the same
    // person on two services.
    assert_eq!(counts.chats, 4);
    assert_eq!(counts.handles, 3);
    assert_eq!(counts.attachments, 1);
    // Nine real messages plus four tapback rows.
    assert_eq!(counts.messages, 13);
}

#[test]
fn no_query_path_can_write_to_the_database() {
    let db = db();
    let write = db
        .conn()
        .execute("UPDATE message SET text = 'nope' WHERE ROWID = 1", []);
    assert!(write.is_err(), "a read-only connection must refuse writes");

    let insert = db.conn().execute(
        "INSERT INTO handle (ROWID, id, service) VALUES (99, 'x', 'iMessage')",
        [],
    );
    assert!(
        insert.is_err(),
        "a read-only connection must refuse inserts"
    );
}

#[test]
fn chats_come_back_newest_first_with_participants_and_counts() {
    let chats = db().chats().expect("chats");
    assert_eq!(chats.len(), 3);

    // The group's last message is newer than the direct chat's, and the empty
    // chat sorts last.
    assert_eq!(chats[0].rowid, fixtures::CHAT_GROUP);
    assert_eq!(chats[1].rowid, fixtures::CHAT_DIRECT);
    assert_eq!(chats[2].rowid, fixtures::CHAT_EMPTY);

    let group = &chats[0];
    assert!(group.is_group);
    assert_eq!(group.participants.len(), 3);
    // Participants keep handle rowid order, which the color assignment needs.
    let order: Vec<i64> = group.participants.iter().map(|h| h.rowid).collect();
    assert_eq!(
        order,
        vec![
            fixtures::HANDLE_ALEX,
            fixtures::HANDLE_BAILEY,
            fixtures::HANDLE_CASEY
        ]
    );
    assert_eq!(group.display_name.as_deref(), Some("Fixture Group"));
    assert_eq!(group.message_count, 3);

    let direct = &chats[1];
    assert!(!direct.is_group);
    assert_eq!(direct.participants.len(), 1);
    assert!(direct.display_name.is_none());
    // Tapback rows must not be counted as messages.
    // Both of the address's `chat` rows, counted once as one conversation.
    assert_eq!(direct.message_count, 6);
    assert_eq!(
        direct.rowid_set(),
        [fixtures::CHAT_DIRECT, fixtures::CHAT_DIRECT_SMS]
    );

    let empty = &chats[2];
    assert_eq!(empty.message_count, 0);
    assert!(empty.last_message_at().is_none());
}

#[test]
fn unread_counts_only_incoming_unread_messages() {
    let db = db();
    let chats = db.chats().expect("chats");
    let direct = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_DIRECT)
        .expect("the direct chat");

    // One incoming message is unread on each of the two rows behind the
    // conversation. The outgoing one, the read ones, and the tapback rows do
    // not count.
    assert_eq!(direct.unread_count, 2);
    assert!(direct.is_unread());

    // The group holds one unread reply; its rename event is not a message and
    // must not be counted as unread.
    let group = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_GROUP)
        .expect("the group chat");
    assert_eq!(group.unread_count, 1);

    let (total, chats_with_unread) = db.unread_totals().expect("totals");
    assert_eq!(total, 3);
    assert_eq!(chats_with_unread, 2);
}

#[test]
fn is_pinned_is_absent_from_this_schema() {
    let db = db();
    assert!(!db.schema().chat_is_pinned);
    assert!(db.schema().tapback_emoji);
    for chat in db.chats().expect("chats") {
        assert!(chat.is_pinned.is_none());
    }
}

#[test]
fn handles_come_back_in_rowid_order() {
    let db = db();
    let handles = db.handles().expect("handles");
    assert_eq!(handles.len(), 3);
    assert!(handles.windows(2).all(|pair| pair[0].rowid < pair[1].rowid));
    assert!(handles[2].is_email());
    assert!(!handles[0].is_email());

    let map = db.handle_map().expect("handle map");
    assert_eq!(
        map.get(&fixtures::HANDLE_CASEY).map(|h| h.service.as_str()),
        Some("iMessage")
    );
}

#[test]
fn a_conversation_page_is_oldest_first_and_excludes_tapback_rows() {
    let page = db()
        .messages_before(&[fixtures::CHAT_DIRECT], None, 50)
        .expect("page");

    let rowids: Vec<i64> = page.iter().map(|message| message.rowid).collect();
    assert_eq!(
        rowids,
        vec![
            fixtures::MSG_PLAIN,
            fixtures::MSG_ATTRIBUTED,
            fixtures::MSG_PHOTO,
            fixtures::MSG_UNREAD,
        ]
    );

    let first = &page[0];
    assert!(!first.is_from_me);
    assert_eq!(first.handle_rowid, Some(fixtures::HANDLE_ALEX));
    assert!(first.handle.is_some());
    assert!(first.sent_at().is_some());
    assert!(!first.is_edited);
    assert!(!first.is_announcement());
}

#[test]
fn paging_walks_backwards_through_a_conversation() {
    let db = db();
    let newest = db
        .messages_before(&[fixtures::CHAT_DIRECT], None, 2)
        .expect("newest page");
    assert_eq!(
        newest.iter().map(|m| m.rowid).collect::<Vec<_>>(),
        vec![fixtures::MSG_PHOTO, fixtures::MSG_UNREAD]
    );

    let older = db
        .messages_before(
            &[fixtures::CHAT_DIRECT],
            Some((newest[0].date, newest[0].rowid)),
            2,
        )
        .expect("older page");
    assert_eq!(
        older.iter().map(|m| m.rowid).collect::<Vec<_>>(),
        vec![fixtures::MSG_PLAIN, fixtures::MSG_ATTRIBUTED]
    );

    let top = db
        .messages_before(
            &[fixtures::CHAT_DIRECT],
            Some((older[0].date, older[0].rowid)),
            2,
        )
        .expect("above the top");
    assert!(top.is_empty());
}

#[test]
fn new_messages_can_be_fetched_by_watermark() {
    let db = db();
    assert_eq!(db.max_message_rowid().expect("watermark"), 21);

    let since = db
        .messages_after(&[fixtures::CHAT_DIRECT], fixtures::MSG_ATTRIBUTED, 50)
        .expect("messages after");
    assert_eq!(
        since.iter().map(|m| m.rowid).collect::<Vec<_>>(),
        vec![fixtures::MSG_PHOTO, fixtures::MSG_UNREAD]
    );
}

#[test]
fn a_body_that_only_lives_in_attributed_body_is_recovered() {
    let page = db()
        .messages_before(&[fixtures::CHAT_DIRECT], None, 50)
        .expect("page");
    let recovered = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_ATTRIBUTED)
        .expect("the attributedBody message");
    assert_eq!(recovered.text.as_deref(), Some(fixtures::ATTRIBUTED_BODY));
}

#[test]
fn an_attachment_only_message_has_a_file_and_no_body() {
    let page = db()
        .messages_before(&[fixtures::CHAT_DIRECT], None, 50)
        .expect("page");
    let photo = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_PHOTO)
        .expect("the photo message");

    assert!(photo.is_from_me);
    // The body was nothing but the attachment placeholder.
    assert!(photo.text.is_none());
    assert!(!photo.is_empty());
    assert_eq!(photo.attachments.len(), 1);

    let file = &photo.attachments[0];
    assert!(file.is_image());
    assert_eq!(file.total_bytes, 2048);
    assert_eq!(file.display_name(), Some("photo.png"));
    assert!(file.path().is_some_and(|path| path.is_absolute()));
    assert!(photo.delivered_at().is_some());
    assert!(photo.read_at().is_some());
}

/// The link fixture's page, oldest first.
fn link_page(db: &Db) -> Vec<msgs::db::Message> {
    db.messages_before(&[fixtures::LINK_CHAT], None, 50)
        .expect("page")
}

#[test]
fn a_link_balloon_hands_back_the_preview_messages_already_stored() {
    let db = Db::open(&fixtures::link_database()).expect("open the link fixture");
    assert!(
        db.schema().link_preview,
        "this schema carries payload_data and balloon_bundle_id"
    );
    let page = link_page(&db);

    let linked = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_LINK)
        .expect("the link message");
    let preview = linked.link_preview.as_ref().expect("a preview");
    assert_eq!(preview.url.as_deref(), Some(fixtures::LINK_URL));
    assert_eq!(preview.title.as_deref(), Some(fixtures::LINK_TITLE));
    assert_eq!(preview.site_name.as_deref(), Some(fixtures::LINK_SITE));
    assert_eq!(preview.summary.as_deref(), Some(fixtures::LINK_SUMMARY));
    assert_eq!(preview.site(), Some(fixtures::LINK_SITE));
    assert_eq!(preview.host(), Some("example.invalid"));
    assert!(!preview.is_empty());

    // The URL is still the body, so the transcript keeps showing it.
    assert_eq!(linked.text.as_deref(), Some(fixtures::LINK_URL));

    // The picture is the second of the message's own attachments, which is
    // where the archive said to look, and it is typed by its bytes rather than
    // by the row Messages wrote.
    let picture = preview.image.as_ref().expect("a preview picture");
    assert_eq!(picture.rowid, linked.attachments[1].rowid);
    assert_eq!(picture.mime_type.as_deref(), Some("image/png"));
    assert!(picture.is_image());
    assert!(!picture.hide_attachment);
    assert_eq!(picture.path(), Some(fixtures::link_image()));
    assert!(
        linked.attachments.iter().all(|file| file.hide_attachment),
        "the payload attachments themselves stay hidden"
    );
}

#[test]
fn an_unreadable_payload_is_no_preview_rather_than_an_error() {
    let db = Db::open(&fixtures::link_database()).expect("open the link fixture");
    let page = link_page(&db);

    let broken = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_LINK_BROKEN)
        .expect("the message with a broken payload");
    assert!(broken.link_preview.is_none());

    let bare = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_LINK_BARE)
        .expect("the message with a bare URL");
    assert!(bare.link_preview.is_none());
    assert_eq!(bare.text.as_deref(), Some(fixtures::BARE_URL));
}

#[test]
fn link_previews_can_be_switched_off_and_the_payload_is_never_read() {
    let mut db = Db::open(&fixtures::link_database()).expect("open the link fixture");
    assert!(db.link_previews(), "on unless somebody says otherwise");
    db.set_link_previews(false);
    assert!(
        link_page(&db)
            .iter()
            .all(|message| message.link_preview.is_none())
    );
}

#[test]
fn a_database_without_the_payload_columns_simply_has_no_previews() {
    // The shared fixture carries the columns; a `chat.db` old enough not to
    // have them must page without asking for them.
    let db = db();
    assert!(db.schema().link_preview);
    assert!(
        db.messages_before(&[fixtures::CHAT_DIRECT], None, 50)
            .expect("page")
            .iter()
            .all(|message| message.link_preview.is_none()),
        "nothing in the shared fixture is a link balloon"
    );
}

#[test]
fn o_on_a_link_opens_it_when_there_is_no_file_to_open() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::link_database());
    app.load_conversation(fixtures::LINK_CHAT);
    app.focus = msgs::app::Focus::Conversation;
    app.messages.selected = 0;

    app.update(Action::OpenAttachment);
    // The browser is not launched in a test environment, so the toast is
    // whichever way `open` went — what matters is that it was not the
    // "no attachment" refusal.
    let toast = app.status.active_toast().map(|(text, _)| text.to_string());
    assert!(
        toast.is_some_and(|text| !text.contains("no attachment")),
        "`o` fell through to the link"
    );
}

#[test]
fn tapbacks_land_on_their_target_with_removals_already_applied() {
    let page = db()
        .messages_before(&[fixtures::CHAT_DIRECT], None, 50)
        .expect("page");
    let target = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_PLAIN)
        .expect("the reacted-to message");

    // One person liked then unliked then laughed; you loved it. Two stand.
    assert_eq!(target.tapbacks.len(), 2);
    assert_eq!(target.tapbacks[0].kind, TapbackKind::Laughed);
    assert!(!target.tapbacks[0].is_from_me);
    assert_eq!(target.tapbacks[1].kind, TapbackKind::Loved);
    assert!(target.tapbacks[1].is_from_me);

    // Everything else in the page is unreacted.
    assert!(
        page.iter()
            .filter(|message| message.rowid != fixtures::MSG_PLAIN)
            .all(|message| message.tapbacks.is_empty())
    );
}

#[test]
fn a_group_rename_reads_as_an_announcement_not_a_message() {
    let page = db()
        .messages_before(&[fixtures::CHAT_GROUP], None, 50)
        .expect("page");
    let rename = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_GROUP_RENAME)
        .expect("the rename event");

    assert!(rename.is_announcement());
    assert_eq!(
        rename.group_action,
        Some(GroupAction::NameChange("Fixture Group".to_string()))
    );

    let reply = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_GROUP_REPLY)
        .expect("the threaded reply");
    assert_eq!(
        reply.thread_originator_guid.as_deref(),
        Some(fixtures::guid(fixtures::MSG_GROUP_FIRST).as_str())
    );
    assert!(reply.group_action.is_none());
}

#[test]
fn an_empty_chat_pages_to_nothing_rather_than_failing() {
    let page = db()
        .messages_before(&[fixtures::CHAT_EMPTY], None, 50)
        .expect("page");
    assert!(page.is_empty());
}

#[test]
fn the_app_loads_the_fixture_and_reports_it_on_the_status_line() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());

    assert_eq!(app.status.db, DbStatus::Ready);
    assert!(app.db_error.is_none());
    // Four `chat` rows, three conversations.
    assert_eq!(app.chat_rows.len(), 3);
    assert_eq!(app.status.unread_total, 3);
    assert_eq!(app.status.unread_chats, 2);

    app.load_conversation(fixtures::CHAT_DIRECT);
    assert_eq!(app.message_rows.len(), 6);
}

#[test]
fn the_app_pages_older_messages_in_above_the_ones_it_has() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());

    let db = app.db.as_ref().expect("an open database");
    app.message_rows = db
        .messages_before(&[fixtures::CHAT_DIRECT], None, 2)
        .expect("newest page");

    assert_eq!(app.load_older_messages(), 2);
    assert_eq!(app.message_rows.len(), 4);
    assert_eq!(app.message_rows[0].rowid, fixtures::MSG_PLAIN);
    // Nothing left above the top.
    assert_eq!(app.load_older_messages(), 0);
}

#[test]
fn a_missing_database_becomes_a_friendly_error_instead_of_a_crash() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(std::path::PathBuf::from("/nonexistent/msgs/chat.db"));

    assert!(app.db.is_none());
    assert!(app.chat_rows.is_empty());
    let err = app.db_error.as_ref().expect("an error to show");
    assert!(!err.headline().is_empty());
    assert!(err.hint().is_some());
    assert_eq!(app.status.db, DbStatus::Unreadable("not found".to_string()));
}

#[test]
fn every_chat_carries_a_preview_of_its_last_message() {
    let chats = db().chats().expect("chats");

    let group = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_GROUP)
        .expect("the group chat");
    assert_eq!(group.last_message_rowid, fixtures::MSG_GROUP_RENAME);
    let preview = group.preview.as_ref().expect("a preview");
    // The newest row in the group is the rename event, not something typed.
    assert!(preview.is_announcement());
    assert!(!preview.is_from_me);
    assert_eq!(preview.sender_rowid, Some(fixtures::HANDLE_BAILEY));

    let direct = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_DIRECT)
        .expect("the direct chat");
    assert_eq!(direct.last_message_rowid, fixtures::MSG_UNREAD);
    let preview = direct.preview.as_ref().expect("a preview");
    assert!(!preview.is_announcement());
    assert!(preview.text.is_some());
    assert_eq!(preview.attachments, 0);

    let empty = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_EMPTY)
        .expect("the empty chat");
    assert_eq!(empty.last_message_rowid, 0);
    assert!(empty.preview.is_none());
}

#[test]
fn a_preview_of_an_attachment_names_the_file_it_carries() {
    let previews = db().previews(&[fixtures::MSG_PHOTO]).expect("previews");
    let preview = previews.get(&fixtures::MSG_PHOTO).expect("the photo");

    assert!(preview.is_from_me);
    // The body was nothing but the attachment placeholder.
    assert!(preview.text.is_none());
    assert_eq!(preview.attachments, 1);
    assert_eq!(preview.attachment_kind, Some(AttachmentKind::Image));
    assert_eq!(preview.attachment_name.as_deref(), Some("photo.png"));
}

#[test]
fn asking_for_no_previews_costs_nothing_and_returns_nothing() {
    assert!(db().previews(&[]).expect("previews").is_empty());
    assert!(db().previews(&[999_999]).expect("previews").is_empty());
}

#[test]
fn opening_the_database_selects_the_newest_chat_and_loads_it() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());

    assert_eq!(app.visible_chats.len(), 3);
    assert_eq!(app.chats.selected, 0);
    // The group holds the newest message, so it opens first.
    assert_eq!(app.open_chat, Some(fixtures::CHAT_GROUP));
    assert_eq!(app.message_rows.len(), 3);
    assert_eq!(app.messages.selected, 2, "the newest message is selected");

    // Moving down the list opens the next conversation.
    app.update(Action::SelectNext);
    assert_eq!(
        app.selected_chat().map(|chat| chat.rowid),
        Some(fixtures::CHAT_DIRECT)
    );
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    assert_eq!(app.message_rows.len(), 6);

    // The empty chat opens to nothing rather than keeping the last one.
    app.update(Action::SelectNext);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_EMPTY));
    assert!(app.message_rows.is_empty());
}

#[test]
fn filtering_the_list_narrows_it_and_opens_what_is_left() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());

    app.update(Action::StartFilter);
    for c in "fixture gr".chars() {
        app.update(Action::Insert(c));
    }
    assert_eq!(app.visible_chats.len(), 1);
    assert_eq!(
        app.selected_chat().map(|chat| chat.rowid),
        Some(fixtures::CHAT_GROUP)
    );

    // A query that matches nothing leaves an empty list and no open chat.
    for c in "zzz".chars() {
        app.update(Action::Insert(c));
    }
    assert!(app.visible_chats.is_empty());
    assert!(app.selected_chat().is_none());
    assert!(app.message_rows.is_empty());

    app.update(Action::Cancel);
    assert_eq!(app.visible_chats.len(), 3);
}

#[test]
fn a_chat_can_say_how_many_files_and_how_many_pictures_it_holds() {
    let db = db();
    assert_eq!(
        db.attachment_counts(&[fixtures::CHAT_DIRECT])
            .expect("counts"),
        (1, 1),
        "one attachment, and it is a picture"
    );
    assert_eq!(
        db.attachment_counts(&[fixtures::CHAT_GROUP])
            .expect("counts"),
        (0, 0)
    );
    assert_eq!(db.attachment_counts(&[999_999]).expect("counts"), (0, 0));
}

#[test]
fn opening_a_chat_measures_it_and_pins_it_to_its_newest_message() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    // The group opens first; give it a pane to be measured against.
    app.prepare_conversation(ratatui::layout::Rect::new(30, 2, 60, 20));

    assert_eq!(app.measured.heights.len(), app.message_rows.len());
    assert!(app.measured.heights.iter().all(|height| *height >= 1));
    assert_eq!(app.measured.by_guid.len(), app.message_rows.len());

    // A conversation shorter than a page has nothing above it, so scrolling up
    // settles rather than asking the database again and again.
    assert!(app.conversation_start_loaded);
    assert_eq!(app.load_older(), 0);

    app.update(Action::SelectNext);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
}

#[test]
fn a_selected_message_can_be_read_back_for_copying() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app.update(Action::SelectNext);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));

    let selected = app.selected_message().expect("the newest message");
    assert_eq!(selected.rowid, fixtures::MSG_UNREAD);
    assert!(!selected.is_from_me);
    assert!(selected.text.is_some());
}

#[test]
fn an_echo_is_retired_when_its_own_row_shows_up_in_the_database() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app.update(Action::SelectNext);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    assert_eq!(app.message_rows.len(), 6);

    // The fixture's own outgoing row carries a file. Rewind the page to just
    // before it and stand an echo of it in its place, which is exactly the
    // state the app is in between pressing Enter and Messages committing the
    // row.
    let sent = app
        .message_rows
        .iter()
        .find(|message| message.rowid == fixtures::MSG_PHOTO)
        .expect("the outgoing row");
    let (date, guid) = (sent.date, sent.guid.clone());
    app.message_rows
        .retain(|message| message.rowid < fixtures::MSG_PHOTO);
    app.messages.set_len(app.message_rows.len());
    app.pending.push(Pending::new(
        0,
        fixtures::CHAT_DIRECT,
        "📎 photo.png".to_string(),
        true,
        date,
    ));
    // An echo in another conversation must not be claimed by this one's rows.
    app.pending.push(Pending::new(
        1,
        fixtures::CHAT_GROUP,
        "📎 photo.png".to_string(),
        true,
        date,
    ));

    assert!(app.tick(), "the arriving rows change the screen");
    assert_eq!(app.pending.len(), 1);
    assert_eq!(app.pending[0].chat_rowid, fixtures::CHAT_GROUP);
    // The real rows are back, and the retired echo is not among them.
    assert_eq!(app.message_rows.len(), 6);
    assert!(app.message_rows.iter().any(|message| message.guid == guid));
    assert!(
        !app.message_rows
            .iter()
            .any(|message| message.guid.starts_with(msgs::send::PENDING_PREFIX)),
        "no echo is left standing in this chat"
    );
}

#[test]
fn an_echo_whose_row_never_arrives_stops_being_a_send_in_progress() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    let open = app.open_chat.expect("an open chat");
    app.pending.push(Pending::new(
        0,
        open,
        "nothing in the fixture matches this".to_string(),
        false,
        msgs::db::raw_time(chrono::Local::now()),
    ));
    // Reopening the conversation puts the echo back on the end of the page.
    app.load_conversation(open);
    assert!(
        app.message_rows
            .last()
            .is_some_and(|message| message.guid.starts_with(msgs::send::PENDING_PREFIX))
    );

    app.tick();
    assert_eq!(app.pending.len(), 1, "nothing claimed it");
    assert_eq!(app.pending[0].state, Delivery::Sending);
    // The echo still stands at the end of the conversation.
    assert!(
        app.message_rows
            .last()
            .is_some_and(|message| message.guid.starts_with(msgs::send::PENDING_PREFIX))
    );
}

#[test]
fn the_two_service_rows_for_one_address_are_one_conversation() {
    let db = db();
    let chats = db.chats().expect("chats");

    // Four `chat` rows, three entries in the list, and the address appears in
    // exactly one of them.
    assert_eq!(chats.len(), 3);
    let holding: Vec<i64> = chats
        .iter()
        .filter(|chat| chat.owns(fixtures::CHAT_DIRECT_SMS))
        .map(|chat| chat.rowid)
        .collect();
    assert_eq!(holding, vec![fixtures::CHAT_DIRECT]);

    let direct = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_DIRECT)
        .expect("the direct conversation");
    // The newest row is the identity, and the other one comes with it.
    assert_eq!(
        direct.rowid_set(),
        [fixtures::CHAT_DIRECT, fixtures::CHAT_DIRECT_SMS]
    );
    assert_eq!(direct.service.as_deref(), Some("iMessage"));
    // One person, not two, however many rows they are spread over.
    assert_eq!(direct.participants.len(), 1);
    // Both rows' unread, counted once.
    assert_eq!(direct.unread_count, 2);
    assert_eq!(direct.message_count, 6);
}

#[test]
fn a_merged_thread_is_both_rows_in_date_order() {
    let page = db()
        .messages_before(
            &[fixtures::CHAT_DIRECT, fixtures::CHAT_DIRECT_SMS],
            None,
            50,
        )
        .expect("the merged page");

    let dates: Vec<i64> = page.iter().map(|message| message.date).collect();
    let mut sorted = dates.clone();
    sorted.sort_unstable();
    assert_eq!(dates, sorted, "a page is drawn oldest first");

    let rowids: Vec<i64> = page.iter().map(|message| message.rowid).collect();
    assert_eq!(
        rowids,
        vec![
            fixtures::MSG_PLAIN,
            fixtures::MSG_ATTRIBUTED,
            fixtures::MSG_PHOTO,
            fixtures::MSG_SMS_READ,
            fixtures::MSG_SMS_UNREAD,
            fixtures::MSG_UNREAD,
        ],
        "the other service's rows are interleaved, not appended"
    );
    // Every row is stamped with the conversation, not with the row it came off.
    assert!(
        page.iter()
            .all(|message| message.chat_rowid == fixtures::CHAT_DIRECT)
    );
}

#[test]
fn opening_the_merged_conversation_loads_both_rows() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app.load_conversation(fixtures::CHAT_DIRECT);
    assert_eq!(app.message_rows.len(), 6);
    assert!(
        app.message_rows
            .iter()
            .any(|message| message.rowid == fixtures::MSG_SMS_UNREAD)
    );
    // Paging upward through a merged thread walks it in date order too.
    let oldest = app.message_rows[3].clone();
    let above = app
        .db
        .as_ref()
        .expect("the database")
        .messages_before(
            &[fixtures::CHAT_DIRECT, fixtures::CHAT_DIRECT_SMS],
            Some((oldest.date, oldest.rowid)),
            50,
        )
        .expect("the page above");
    assert_eq!(
        above
            .iter()
            .map(|message| message.rowid)
            .collect::<Vec<_>>(),
        vec![
            fixtures::MSG_PLAIN,
            fixtures::MSG_ATTRIBUTED,
            fixtures::MSG_PHOTO
        ]
    );
}
