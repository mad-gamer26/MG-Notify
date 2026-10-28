package com.matthewdovi.mgnotify

import android.util.Patterns
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import org.json.JSONArray
import org.json.JSONObject
import java.text.DateFormat
import java.time.Instant
import java.util.Date

data class SavedAccount(
    val accountID: String,
    val email: String,
    val sessionToken: String,
    /** Optional; empty when the account has none. */
    val displayName: String = "",
) {
    /** What to call the account: its display name, or else its email. */
    val name: String get() = displayName.ifEmpty { email }

    fun toJson(): JSONObject = JSONObject()
        .put("accountID", accountID)
        .put("email", email)
        .put("sessionToken", sessionToken)
        .put("displayName", displayName)

    companion object {
        fun listFromJson(text: String?): List<SavedAccount> {
            if (text.isNullOrEmpty()) return emptyList()
            return runCatching {
                val array = JSONArray(text)
                (0 until array.length()).map { index ->
                    val item = array.getJSONObject(index)
                    SavedAccount(
                        accountID = item.getString("accountID"),
                        email = item.getString("email"),
                        sessionToken = item.getString("sessionToken"),
                        displayName = item.optString("displayName", ""),
                    )
                }
            }.getOrDefault(emptyList())
        }

        fun listToJson(accounts: List<SavedAccount>): String =
            JSONArray(accounts.map { it.toJson() }).toString()
    }
}

enum class PushHandlingResult { IGNORED, REFRESHED, REMOTE_EVENT }

enum class PushEvent(val rawValue: String) {
    SIGNED_OUT("signed_out"),
    ACCOUNT_DELETED("account_deleted"),
    DEVICES_UPDATED("devices_updated"),
    API_KEYS_UPDATED("api_keys_updated"),
    NOTIFICATIONS_UPDATED("notifications_updated"),
    EMAIL_VERIFIED("email_verified"),
    EMAIL_VERIFICATION_REQUIRED("email_verification_required");

    companion object {
        fun from(rawValue: String?): PushEvent? = entries.firstOrNull { it.rawValue == rawValue }
    }
}

class RequestError(val statusCode: Int?, message: String) : Exception(message)

enum class AuthScreenMode(val title: String) {
    SIGN_IN("Sign In"),
    CREATE_ACCOUNT("Create Account"),
    FORGOT_PASSWORD("Forgot Password"),
}

data class SoundOption(val id: String, val title: String)

private val dateFormatter: DateFormat = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT)

/** The server's ISO 8601 timestamps, shown like the iOS app's medium date, short time. */
fun formatServerDate(value: String): String =
    runCatching { dateFormatter.format(Date.from(Instant.parse(value))) }.getOrDefault(value)

data class AccountDevice(
    val deviceID: String,
    val deviceName: String,
    val platform: String,
    val lastIPAddress: String,
    val lastSeen: String,
) {
    val formattedLastSeen: String get() = formatServerDate(lastSeen)

    companion object {
        fun fromJson(json: JSONObject) = AccountDevice(
            deviceID = json.getString("device_id"),
            deviceName = json.getString("device_name"),
            platform = json.getString("platform"),
            lastIPAddress = json.optString("last_ip_address"),
            lastSeen = json.optString("last_seen"),
        )
    }
}

data class ManagedAPIKey(
    val keyID: String,
    val label: String,
    val apiKey: String,
    val createdAt: String?,
    val lastUsedAt: String?,
) {
    companion object {
        fun fromJson(json: JSONObject) = ManagedAPIKey(
            keyID = json.getString("key_id"),
            label = json.getString("label"),
            apiKey = json.getString("api_key"),
            createdAt = json.optStringOrNull("created_at"),
            lastUsedAt = json.optStringOrNull("last_used_at"),
        )
    }
}

fun JSONObject.optStringOrNull(name: String): String? =
    if (isNull(name)) null else optString(name)

data class NotificationLink(val id: String, val displayText: String, val url: String) {
    val accessibilityText: String get() = "Link: $displayText"
}

class ParsedNotificationMessage(
    val text: AnnotatedString,
    val accessibilityText: String,
    val htmlText: String,
    val links: List<NotificationLink>,
)

data class AccountNotification(
    val notificationID: String,
    val title: String,
    val message: String,
    val createdAt: String,
    val email: Boolean,
    val html: String?,
    /** False until the notification list has been opened on any device. */
    val seen: Boolean = true,
) {
    val isEmailNotification: Boolean get() = email

    val displayTitle: String
        get() {
            if (!isEmailNotification) return title
            val range = emailAddressBlockRange ?: return normalizedEmailHeaderText(title)
            return normalizedEmailHeaderText(title.removeRange(range))
        }

    val emailSenderName: String
        get() {
            val range = emailAddressBlockRange
                ?: return normalizedEmailHeaderText(title.substringBefore(":"))
            val name = normalizedEmailHeaderText(title.substring(0, range.first))
            return name.ifEmpty { emailSenderAddress }
        }

    val emailSenderAddress: String
        get() {
            val range = emailAddressBlockRange ?: return ""
            return title.substring(range.first + 1, range.last).trim()
        }

    val emailSubject: String
        get() {
            val colon = title.indexOf(':')
            if (colon < 0) return title
            return title.substring(colon + 1).trim().ifEmpty { title }
        }

    val emailHTML: String get() = html ?: message

    /** The `<address>` part of an email title, brackets included. */
    private val emailAddressBlockRange: IntRange?
        get() {
            val start = title.indexOf('<')
            if (start < 0) return null
            val end = title.indexOf('>', start)
            if (end < 0) return null
            return start..end
        }

    val formattedDate: String get() = formatServerDate(createdAt)

    val parsedMessage: ParsedNotificationMessage by lazy { NotificationMessageParser.parse(message) }

    val plainTextForCopy: String
        get() = listOf(title, message, htmlSafeDateText(formattedDate)).joinToString("\n")

    val richHTMLForCopy: String
        get() = """
            <div>
            <p><strong>${escapeHTML(title)}</strong></p>
            <p>${parsedMessage.htmlText}</p>
            <p>${escapeHTML(htmlSafeDateText(formattedDate))}</p>
            </div>
        """.trimIndent()

    companion object {
        fun fromJson(json: JSONObject) = AccountNotification(
            notificationID = json.getString("notification_id"),
            title = json.getString("title"),
            message = json.getString("message"),
            createdAt = json.getString("created_at"),
            email = json.optBoolean("email", false),
            html = json.optStringOrNull("html"),
            seen = json.optBoolean("seen", true),
        )

        private fun normalizedEmailHeaderText(text: String): String =
            text.replace("  ", " ").replace(" :", ":").trim()

        private fun htmlSafeDateText(text: String): String =
            text.replace(' ', ' ').replace(' ', ' ')
    }
}

fun escapeHTML(text: String): String = text
    .replace("&", "&amp;")
    .replace("<", "&lt;")
    .replace(">", "&gt;")
    .replace("\"", "&quot;")

/**
 * Turns a notification's text into display text with tappable links: Markdown
 * links (`[title](url)`), web addresses, email addresses and any `scheme:thing`
 * link, the same set the iOS app recognises.
 */
object NotificationMessageParser {
    private val markdownLink = Regex("""\[([^\]]+)\]\(([^)]+)\)""")
    private val customLink = Regex("""\b[A-Za-z][A-Za-z0-9+\-.]*:[^\s<>()]+""")
    private const val TRIM_CHARACTERS = ".,;:!?)]}>\"'"
    private const val MARKDOWN_URL_ALLOWED =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~:/?[]@!\$&'()*+,;="
    val linkColor = Color(0xFF007AFF)

    private class Builder {
        val text = AnnotatedString.Builder()
        val accessibility = StringBuilder()
        val html = StringBuilder()
        val links = mutableListOf<NotificationLink>()

        fun appendPlain(segment: String) {
            text.append(segment)
            accessibility.append(segment)
            html.append(escapeHTML(segment).replace("\n", "<br>"))
        }

        fun appendLink(displayText: String, url: String) {
            val link = NotificationLink("link-${links.size}", displayText, url)
            text.withLink(
                LinkAnnotation.Url(
                    url,
                    TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)),
                ),
            ) { append(displayText) }
            accessibility.append(link.accessibilityText)
            html.append("<a href=\"${escapeHTML(url)}\">${escapeHTML(displayText)}</a>")
            links += link
        }
    }

    fun parse(message: String): ParsedNotificationMessage {
        val builder = Builder()
        var position = 0
        for (match in markdownLink.findAll(message)) {
            appendPlainSegment(builder, message.substring(position, match.range.first))
            val title = match.groupValues[1]
            val url = percentEncodedMarkdownURL(match.groupValues[2].trim { it in TRIM_CHARACTERS })
            if (url.isNotEmpty()) {
                builder.appendLink(title, url)
            } else {
                appendPlainSegment(builder, match.value)
            }
            position = match.range.last + 1
        }
        appendPlainSegment(builder, message.substring(position))
        return ParsedNotificationMessage(
            text = builder.text.toAnnotatedString(),
            accessibilityText = builder.accessibility.toString(),
            htmlText = builder.html.toString(),
            links = builder.links,
        )
    }

    private data class LinkMatch(val start: Int, val end: Int, val text: String, val url: String)

    private fun appendPlainSegment(builder: Builder, segment: String) {
        if (segment.isEmpty()) return
        var position = 0
        for (match in linkMatches(segment)) {
            if (match.start > position) builder.appendPlain(segment.substring(position, match.start))
            builder.appendLink(match.text, match.url)
            position = match.end
        }
        if (position < segment.length) builder.appendPlain(segment.substring(position))
    }

    private fun linkMatches(text: String): List<LinkMatch> {
        val candidates = mutableListOf<Pair<IntRange, Boolean>>()
        val web = Patterns.WEB_URL.matcher(text)
        while (web.find()) candidates += (web.start() until web.end()) to false
        val emails = Patterns.EMAIL_ADDRESS.matcher(text)
        while (emails.find()) candidates += (emails.start() until emails.end()) to true
        customLink.findAll(text).forEach { candidates += it.range to false }

        val matches = mutableListOf<LinkMatch>()
        for ((range, isEmail) in candidates.sortedBy { it.first.first }) {
            val raw = text.substring(range)
            val leading = raw.indexOfFirst { it !in TRIM_CHARACTERS }
            if (leading < 0) continue
            val trimmed = raw.trim { it in TRIM_CHARACTERS }
            val start = range.first + leading
            val end = start + trimmed.length
            if (matches.any { start < it.end && it.start < end }) continue
            matches += LinkMatch(start, end, trimmed, linkURL(trimmed, isEmail))
        }
        return matches.sortedBy { it.start }
    }

    /** Web addresses written without a scheme open as https, email addresses as mailto. */
    private fun linkURL(text: String, isEmail: Boolean): String = when {
        isEmail && !text.startsWith("mailto:", ignoreCase = true) -> "mailto:$text"
        Regex("^[A-Za-z][A-Za-z0-9+\\-.]*:").containsMatchIn(text) -> text
        else -> "https://$text"
    }

    private fun percentEncodedMarkdownURL(text: String): String {
        val out = StringBuilder()
        var index = 0
        while (index < text.length) {
            val char = text[index]
            if (char == '%' && index + 2 <= text.lastIndex &&
                text[index + 1].isHexDigit() && text[index + 2].isHexDigit()
            ) {
                out.append(text, index, index + 3)
                index += 3
                continue
            }
            val codePoint = text.codePointAt(index)
            val charCount = Character.charCount(codePoint)
            if (charCount == 1 && char in MARKDOWN_URL_ALLOWED) {
                out.append(char)
            } else {
                String(Character.toChars(codePoint)).toByteArray(Charsets.UTF_8).forEach { byte ->
                    out.append("%%%02X".format(byte.toInt() and 0xFF))
                }
            }
            index += charCount
        }
        return out.toString()
    }

    private fun Char.isHexDigit() = this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'
}
