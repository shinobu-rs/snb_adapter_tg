A telegram adapter for shinobu

Built on frankenstein 0.50.1 (Bot API 10.1), with a hand-rolled long-poll loop.

## Action channel

Other plugins can call whitelisted Telegram methods through the adapter without
any core ABI surface: emit an event with `event_type = Other("tg:<method>")`,
`target_plugin = "TGAdapter"`, and `data` = the Bot API parameters as JSON
(exact Bot API field names). Supported methods: `answer_chat_join_request_query`,
`send_chat_join_request_web_app`, `approve_chat_join_request`,
`decline_chat_join_request`, `restrict_chat_member`, `ban_chat_member`,
`unban_chat_member`, `answer_callback_query`, `get_chat`.
If `reply_plugin` is set, the adapter answers with `Other("tg:response")`
(`data = {"method", "ok", "result"|"error"}`).
