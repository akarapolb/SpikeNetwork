package network.spike.connect

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.semantics.selected
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.material3.LocalTextStyle
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/**
 * The palette, shared with the desktop app (desktop/ui/app.css): "quiet ink, one signal". Actions and your own
 * messages are ink; colour means status only: teal ([Green]) = verified or encrypted, amber = waiting for you,
 * red = danger. The name [Accent] is kept so existing screens keep working; its value is now ink, not teal.
 * People's avatars get a soft tint so they are easier to tell apart. Light and dark follow the phone.
 */
object C {
    /** Set by [SpikeTheme]; a composable that reads a colour recomposes when this changes. */
    var dark by mutableStateOf(false)
    private fun p(light: Long, dark: Long) = Color(if (this.dark) dark else light)

    // Phone screens are white in the design (the desktop keeps a grey page behind its panels).
    val Bg get() = p(0xFFFFFFFF, 0xFF111516)
    val BgChat get() = p(0xFFFFFFFF, 0xFF111516)
    val Surface get() = p(0xFFFFFFFF, 0xFF111516)
    /** Filled inputs, chips, incoming bubbles and the composer. */
    val Subtle get() = p(0xFFEEF0F0, 0xFF1E2427)
    val Border get() = p(0xFFE4E7E8, 0xFF22292B)
    val BorderStrong get() = p(0xFFCFD5D7, 0xFF343D40)
    val Text get() = p(0xFF0D1416, 0xFFF0F3F3)
    val Muted get() = p(0xFF657175, 0xFF8C989C)
    val AvatarBg get() = p(0xFFEEF0F0, 0xFF1E2427)
    val AvatarText get() = p(0xFF4B565A, 0xFFB4BEC1)
    /** Ink: buttons, selection, your own messages. */
    val Accent get() = p(0xFF0D1416, 0xFFF0F3F3)
    /** Text and icons on a filled accent background. */
    val OnAccent get() = p(0xFFFFFFFF, 0xFF0A0D0E)
    val AccentSoft get() = p(0xFFEEF0F0, 0xFF1E2427)
    val AccentSoftText get() = p(0xFF0D1416, 0xFFF0F3F3)
    val BubbleOut get() = p(0xFF0D1416, 0xFFF0F3F3)
    val OnBubbleOut get() = p(0xFFFFFFFF, 0xFF0A0D0E)
    /** The one signal colour: verified, encrypted, connected. Teal, despite the name. */
    val Green get() = p(0xFF00786F, 0xFF56CFC1)
    val AmberText get() = p(0xFF7A4A00, 0xFFF0CB86)
    val AmberDeep get() = p(0xFF4A3200, 0xFFF0CB86)
    val AmberBg get() = p(0xFFFAF0DA, 0xFF2E2410)
    val AmberBorder get() = p(0xFFE4C98F, 0xFF9A7330)
    val RedText get() = p(0xFF6E1F1C, 0xFFF6C4C1)
    val RedBg get() = p(0xFFF8E3E0, 0xFF3A1B1A)
    val Danger get() = p(0xFFB93A2E, 0xFFF0867B)
    /** The app mark keeps its own colours in both themes. */
    val MarkBg = Color(0xFF0B1113)
    val Mark = Color(0xFF2FEBD3)

    private val tintsLight = listOf(
        0xFFDCEBF7 to 0xFF24527A, 0xFFE5E0F5 to 0xFF4B3C85, 0xFFF5E1EA to 0xFF7A2D52,
        0xFFF6E7D6 to 0xFF7A4A1C, 0xFFDDF0E4 to 0xFF275C3A, 0xFFDDEFF0 to 0xFF1D5C61,
    )
    private val tintsDark = listOf(
        0xFF1E3346 to 0xFFB5D3EE, 0xFF2B2747 to 0xFFCFC5F2, 0xFF3A2330 to 0xFFF0C2D6,
        0xFF3A2C1D to 0xFFF2D2AE, 0xFF1C3326 to 0xFFBFE6CC, 0xFF183538 to 0xFFA9DEE2,
    )

    /** A soft background and text colour picked from a name: the same pick as the desktop app's tint(). */
    fun tint(name: String, dark: Boolean = this.dark): Pair<Color, Color> {
        var n = 0L
        name.codePoints().forEach { n = (n * 31 + it) and 0xFFFFFFFFL }
        val t = (if (dark) tintsDark else tintsLight)[(n % 6).toInt()]
        return Color(t.first) to Color(t.second)
    }
}

/** Geist and Geist Mono (SIL OFL 1.1, see android/FONTS-LICENSE.txt), the design's faces. Thai and other scripts Geist
 *  does not cover fall back to the phone's own fonts. */
val Geist = FontFamily(
    Font(R.font.geist_regular, FontWeight.Normal),
    Font(R.font.geist_medium, FontWeight.Medium),
    Font(R.font.geist_semibold, FontWeight.SemiBold),
    Font(R.font.geist_semibold, FontWeight.Bold),
)
val Mono = FontFamily(
    Font(R.font.geist_mono_regular, FontWeight.Normal),
    Font(R.font.geist_mono_medium, FontWeight.Medium),
)

@Composable
fun SpikeTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    C.dark = dark
    // Filled in beyond primary so stock Material parts (switches, checkboxes, dialogs, snackbars) use the palette
    // instead of Material's default purple-grey.
    val scheme = (if (dark) darkColorScheme() else lightColorScheme()).copy(
        primary = C.Accent, onPrimary = C.OnAccent, primaryContainer = C.AccentSoft, onPrimaryContainer = C.AccentSoftText,
        secondary = C.Accent, onSecondary = C.OnAccent, secondaryContainer = C.Subtle, onSecondaryContainer = C.Text,
        tertiary = C.Green, background = C.Bg, onBackground = C.Text, surface = C.Surface, onSurface = C.Text,
        surfaceVariant = C.Subtle, onSurfaceVariant = C.Muted, surfaceTint = C.Surface,
        surfaceContainerLowest = C.Surface, surfaceContainerLow = C.Surface, surfaceContainer = C.Surface,
        surfaceContainerHigh = C.Subtle, surfaceContainerHighest = C.BorderStrong,
        inverseSurface = C.Accent, inverseOnSurface = C.OnAccent, inversePrimary = C.Mark,
        // outline is also an off switch's thumb and border, so it must stand out from the track (surfaceContainerHighest).
        outline = C.Muted, outlineVariant = C.Border, error = C.Danger, onError = C.OnAccent,
    )
    MaterialTheme(colorScheme = scheme) {
        CompositionLocalProvider(
            LocalTextStyle provides TextStyle(fontFamily = Geist, fontSize = 15.sp, color = C.Text),
        ) { content() }
    }
}

@Composable
fun Ico(icon: ImageVector, tint: Color = C.Text, size: Dp = 22.dp, modifier: Modifier = Modifier) {
    Icon(icon, contentDescription = null, tint = tint, modifier = modifier.size(size))
}

/** Heading text: Geist SemiBold, tracked in as in the design. */
@Composable
fun H1(text: String, size: TextUnit = 30.sp, modifier: Modifier = Modifier) {
    Text(text, fontFamily = Geist, fontWeight = FontWeight.SemiBold, fontSize = size, letterSpacing = (-0.035).em, lineHeight = size * 1.1, modifier = modifier)
}

@Composable
fun Muted(text: String, size: TextUnit = 13.sp, modifier: Modifier = Modifier) {
    Text(text, color = C.Muted, fontSize = size, modifier = modifier)
}

/** Initials in a neutral circle (a person) or rounded square (a group, an agent), as in the design: people are
 *  told apart by their initials, colour is kept for status. */
@Composable
fun Avatar(text: String, size: Dp = 46.dp, square: Boolean = false) {
    val initials = text.split(' ', '-', '_').filter { it.isNotEmpty() }.take(2)
        .joinToString("") { it.take(1).uppercase() }.ifEmpty { "?" }
    val (bg, fg) = C.AvatarBg to C.AvatarText
    Box(
        Modifier.size(size).clip(if (square) RoundedCornerShape(size * 0.3f) else CircleShape).background(bg),
        contentAlignment = Alignment.Center,
    ) {
        Text(initials, color = fg, fontWeight = FontWeight.SemiBold, fontSize = (size.value * 0.36f).sp)
    }
}

/** An empty state: a quiet icon above the same sentence as before. */
@Composable
fun EmptyState(icon: ImageVector, text: String, modifier: Modifier = Modifier) {
    Column(
        modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 40.dp),
        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Box(Modifier.size(64.dp).clip(RoundedCornerShape(22.dp)).background(C.AccentSoft), contentAlignment = Alignment.Center) {
            Ico(icon, C.AccentSoftText, 28.dp)
        }
        Text(text, color = C.Muted, fontSize = 14.5.sp, textAlign = TextAlign.Center)
    }
}

enum class TagKind { Neutral, Red, Amber }

@Composable
fun Tag(text: String, kind: TagKind = TagKind.Neutral) {
    val (fg, bg) = when (kind) {
        TagKind.Red -> C.RedText to C.RedBg
        TagKind.Amber -> C.AmberText to C.AmberBg
        TagKind.Neutral -> C.AvatarText to C.AvatarBg
    }
    Text(
        text, color = fg, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
        modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(bg).padding(horizontal = 8.dp, vertical = 1.dp),
    )
}

/** A rounded pill with an optional dot or icon, like the status chips on the chat list. */
@Composable
fun Chip(text: String, dot: Color? = null, icon: ImageVector? = null, onClick: () -> Unit = {}) {
    Row(
        Modifier.heightIn(min = 32.dp).clip(RoundedCornerShape(999.dp)).background(C.Subtle).clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (dot != null) Box(Modifier.size(8.dp).clip(CircleShape).background(dot))
        if (icon != null) Ico(icon, C.AvatarText, 14.dp)
        Text(text, fontSize = 12.5.sp, color = C.AvatarText)
    }
}

@Composable
fun PrimaryButton(
    text: String, enabled: Boolean = true, modifier: Modifier = Modifier, icon: ImageVector? = null, onClick: () -> Unit,
) {
    Row(
        modifier.fillMaxWidth().heightIn(min = 52.dp).clip(RoundedCornerShape(14.dp))
            .background(if (enabled) C.Accent else C.Accent.copy(alpha = 0.4f))
            .clickable(enabled = enabled, onClick = onClick),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
    ) {
        if (icon != null) Ico(icon, C.OnAccent, 20.dp)
        Text(text, color = C.OnAccent, fontWeight = FontWeight.SemiBold, fontSize = 16.sp)
    }
}

@Composable
fun OutlineButton(
    text: String, modifier: Modifier = Modifier, enabled: Boolean = true, danger: Boolean = false,
    small: Boolean = false, onClick: () -> Unit,
) {
    Box(
        modifier.heightIn(min = if (small) 36.dp else 48.dp).clip(RoundedCornerShape(if (small) 10.dp else 14.dp))
            .background(C.Surface).border(BorderStroke(1.dp, C.BorderStrong), RoundedCornerShape(if (small) 10.dp else 14.dp))
            .clickable(enabled = enabled, onClick = onClick).padding(horizontal = if (small) 12.dp else 16.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text, fontWeight = if (small) FontWeight.SemiBold else FontWeight.Medium,
            fontSize = if (small) 13.sp else 15.sp,
            color = (if (danger) C.Danger else C.Text).copy(alpha = if (enabled) 1f else 0.5f),
        )
    }
}

/** A labelled text input in the prototype's style: 48dp, 12dp radius, strong border, helper below. */
@Composable
fun Field(
    label: String?, value: String, onChange: (String) -> Unit, modifier: Modifier = Modifier,
    helper: String? = null, password: Boolean = false, singleLine: Boolean = true, minHeight: Dp = 48.dp,
    placeholder: String? = null, keyboard: KeyboardOptions = KeyboardOptions.Default,
    maxLines: Int = if (singleLine) 1 else Int.MAX_VALUE,
) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (label != null) Text(label, fontSize = 14.sp, fontWeight = FontWeight.Medium)
        Box(
            Modifier.fillMaxWidth().heightIn(min = minHeight).clip(RoundedCornerShape(12.dp)).background(C.Surface)
                .border(1.dp, C.BorderStrong, RoundedCornerShape(12.dp)).padding(horizontal = 14.dp, vertical = 12.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            BasicTextField(
                value, onChange, singleLine = singleLine, maxLines = maxLines,
                textStyle = TextStyle(fontFamily = Geist, fontSize = 16.sp, color = C.Text),
                cursorBrush = SolidColor(C.Accent),
                visualTransformation = if (password) PasswordVisualTransformation() else VisualTransformation.None,
                keyboardOptions = keyboard, modifier = Modifier.fillMaxWidth(),
                decorationBox = { inner ->
                    if (value.isEmpty() && placeholder != null) Text(placeholder, color = C.Muted, fontSize = 16.sp)
                    inner()
                },
            )
        }
        if (helper != null) Muted(helper, 12.5.sp)
    }
}

/** A section of a page: flat, with a hairline under it, as in the design (no boxes inside boxes). */
@Composable
fun CardBox(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(modifier.fillMaxWidth()) {
        Column(Modifier.fillMaxWidth().padding(top = 6.dp, bottom = 16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) { content() }
        Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
    }
}

/** A bordered card, for the one object a screen is about (an agent's request). Amber while it waits for you. */
@Composable
fun FramedBox(waiting: Boolean = false, modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    val shape = RoundedCornerShape(18.dp)
    Column(
        modifier.fillMaxWidth().clip(shape).background(C.Surface)
            .border(1.dp, if (waiting) C.AmberBorder else C.BorderStrong, shape).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) { content() }
}

/** The title row used by every sub-screen: a 44dp back button and the title. */
@Composable
fun TopBar(title: String, onBack: (() -> Unit)?, sub: String? = null, trailing: @Composable () -> Unit = {}) {
    Row(
        Modifier.fillMaxWidth().background(C.Surface).statusBarsPadding().padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (onBack != null) {
            Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onBack).semantics { contentDescription = "Back" }, contentAlignment = Alignment.Center) {
                Ico(Ic.Back)
            }
        }
        Column(Modifier.weight(1f)) {
            Text(title, fontFamily = Geist, fontWeight = FontWeight.SemiBold, fontSize = 20.sp, maxLines = 1)
            if (sub != null) Text(sub, color = C.Muted, fontSize = 12.5.sp, maxLines = 1)
        }
        trailing()
    }
    Box(Modifier.fillMaxWidth().size(1.dp).background(C.Border))
}

/** A screen that is one step of a flow, as in the design: a back button and "2 of 3" on a borderless top row, a large
 *  heading and one sentence, the step's content, and the actions pinned to the bottom. */
@Composable
fun StepPage(
    onBack: () -> Unit, count: String? = null, title: String? = null, body: String? = null,
    actions: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit = {},
    content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit,
) {
    Column(Modifier.fillMaxSize().background(C.Surface).statusBarsPadding().navigationBarsPadding()) {
        Row(Modifier.fillMaxWidth().padding(start = 6.dp, end = 20.dp, top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onBack).semantics { contentDescription = "Back" }, contentAlignment = Alignment.Center) {
                Ico(Ic.Back, C.AvatarText)
            }
            Spacer(Modifier.weight(1f))
            if (count != null) Text(count, color = C.Muted, fontSize = 12.5.sp)
        }
        Column(
            Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()).padding(start = 24.dp, end = 24.dp, top = 8.dp, bottom = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (title != null) H1(title, 26.sp)
            if (body != null) Text(body, color = C.AvatarText, fontSize = 14.sp, lineHeight = 19.6.sp)
            content()
        }
        Column(Modifier.fillMaxWidth().padding(start = 24.dp, end = 24.dp, bottom = 20.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) { actions() }
    }
}

/** A centred text action under a primary button ("They differ, cancel", "Decline"). */
@Composable
fun TextLink(text: String, danger: Boolean = false, onClick: () -> Unit) {
    Text(
        text, color = if (danger) C.Danger else C.AvatarText, fontSize = 13.5.sp, textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).clickable(onClick = onClick).padding(vertical = 12.dp),
    )
}

/** A label and its value on one line, with a hairline above, as in the design's key-value lists. */
@Composable
fun KvRow(label: String, value: String) {
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
    Row(Modifier.fillMaxWidth().padding(vertical = 9.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(label, color = C.Muted, fontSize = 13.sp)
        Text(value, fontSize = 13.sp, textAlign = TextAlign.End, modifier = Modifier.weight(1f))
    }
}

/** A choice among a few, as a bordered row with a radio dot; the chosen one gets an ink border. */
@Composable
fun OptionRow(title: String, desc: String, on: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(14.dp)
    Row(
        Modifier.fillMaxWidth().clip(shape).border(if (on) 2.dp else 1.dp, if (on) C.Text else C.BorderStrong, shape)
            .clickable(onClick = onClick).semantics { selected = on }.padding(horizontal = 14.dp, vertical = 11.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.padding(top = 1.dp).size(18.dp).clip(CircleShape).border(if (on) 5.dp else 1.5.dp, if (on) C.Text else C.BorderStrong, CircleShape))
        Column(Modifier.weight(1f)) {
            Text(title, fontWeight = FontWeight.Medium, fontSize = 14.sp)
            Text(desc, color = C.Muted, fontSize = 12.5.sp)
        }
    }
}

/** A code to compare, large and spaced on a sunken panel. */
@Composable
fun CodePanel(code: String) {
    Box(Modifier.fillMaxWidth().padding(top = 10.dp).clip(RoundedCornerShape(16.dp)).background(C.Subtle).padding(vertical = 22.dp), contentAlignment = Alignment.Center) {
        Text(code, fontFamily = Mono, fontWeight = FontWeight.Medium, fontSize = 30.sp, letterSpacing = 0.12.em, textAlign = TextAlign.Center)
    }
}

/** Small text with a leading icon, for list items such as "End-to-end encrypted. ..." */
@Composable
fun Bullet(icon: ImageVector, bold: String, rest: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.Top) {
        Ico(icon, C.Accent, 24.dp)
        Text(
            androidx.compose.ui.text.buildAnnotatedString {
                pushStyle(androidx.compose.ui.text.SpanStyle(fontWeight = FontWeight.SemiBold))
                append(bold)
                pop()
                append(" ")
                append(rest)
            },
            fontSize = 14.5.sp, modifier = Modifier.widthIn(max = 320.dp),
        )
    }
}
