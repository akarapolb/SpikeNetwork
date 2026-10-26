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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
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
import androidx.compose.ui.unit.sp

/**
 * The palette, shared with the desktop app (desktop/ui/app.css): refined teal, colour means status,
 * and people's avatars get a soft tint so they are easier to tell apart. Light and dark follow the phone.
 */
object C {
    /** Set by [SpikeTheme]; a composable that reads a colour recomposes when this changes. */
    var dark by mutableStateOf(false)
    private fun p(light: Long, dark: Long) = Color(if (this.dark) dark else light)

    val Bg get() = p(0xFFF4F6F8, 0xFF0E1417)
    val BgChat get() = p(0xFFEEF2F5, 0xFF111A1E)
    val Surface get() = p(0xFFFFFFFF, 0xFF162026)
    /** Filled inputs, chips and the composer. */
    val Subtle get() = p(0xFFEDF1F4, 0xFF1D2930)
    val Border get() = p(0xFFE1E6EB, 0xFF26343C)
    val BorderStrong get() = p(0xFFCBD4DC, 0xFF34454F)
    val Text get() = p(0xFF13202A, 0xFFE6EDF2)
    val Muted get() = p(0xFF5B6875, 0xFF95A4AF)
    val AvatarBg get() = p(0xFFE8EDF1, 0xFF22303A)
    val AvatarText get() = p(0xFF3A4753, 0xFFC3CED7)
    val Accent get() = p(0xFF0E7C86, 0xFF5CC2C9)
    /** Text and icons on a filled accent background. */
    val OnAccent get() = p(0xFFFFFFFF, 0xFF062A2D)
    val AccentSoft get() = p(0xFFE0F1F2, 0xFF15353A)
    val AccentSoftText get() = p(0xFF0A5A61, 0xFFA3E0E4)
    val BubbleOut get() = p(0xFF0E7C86, 0xFF1F7F87)
    val OnBubbleOut = Color.White
    val Green get() = p(0xFF1F7A45, 0xFF6FCF97)
    val AmberText get() = p(0xFF7A4A00, 0xFFF5DDB0)
    val AmberDeep get() = p(0xFF4A3200, 0xFFF5DDB0)
    val AmberBg get() = p(0xFFFBEED0, 0xFF3A2C10)
    val AmberBorder get() = p(0xFFE4C98F, 0xFF9A7330)
    val RedText get() = p(0xFF6E1F1C, 0xFFF6C4C1)
    val RedBg get() = p(0xFFF8E1E1, 0xFF3A1B1A)
    val Danger get() = p(0xFFA0302B, 0xFFF08A84)

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

val Plex = FontFamily(
    Font(R.font.plex_sans_regular, FontWeight.Normal),
    Font(R.font.plex_sans_medium, FontWeight.Medium),
    Font(R.font.plex_sans_semibold, FontWeight.SemiBold),
    Font(R.font.plex_sans_semibold, FontWeight.Bold),
)
val Grotesk = FontFamily(
    Font(R.font.grotesk_semibold, FontWeight.SemiBold),
    Font(R.font.grotesk_bold, FontWeight.Bold),
)
val Mono = FontFamily(Font(R.font.plex_mono_regular, FontWeight.Normal))

@Composable
fun SpikeTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    C.dark = dark
    val scheme = if (dark) darkColorScheme(primary = C.Accent, onPrimary = C.OnAccent, background = C.Bg, surface = C.Surface, onSurface = C.Text, onBackground = C.Text)
    else lightColorScheme(primary = C.Accent, onPrimary = C.OnAccent, background = C.Bg, surface = C.Surface, onSurface = C.Text, onBackground = C.Text)
    MaterialTheme(colorScheme = scheme) {
        CompositionLocalProvider(
            LocalTextStyle provides TextStyle(fontFamily = Plex, fontSize = 15.sp, color = C.Text),
        ) { content() }
    }
}

@Composable
fun Ico(icon: ImageVector, tint: Color = C.Text, size: Dp = 22.dp, modifier: Modifier = Modifier) {
    Icon(icon, contentDescription = null, tint = tint, modifier = modifier.size(size))
}

/** Heading text in the prototype's display face. */
@Composable
fun H1(text: String, size: TextUnit = 30.sp, modifier: Modifier = Modifier) {
    Text(text, fontFamily = Grotesk, fontWeight = FontWeight.SemiBold, fontSize = size, lineHeight = size * 1.1, modifier = modifier)
}

@Composable
fun Muted(text: String, size: TextUnit = 13.sp, modifier: Modifier = Modifier) {
    Text(text, color = C.Muted, fontSize = size, modifier = modifier)
}

/** Initials in a softly tinted circle (a person) or a neutral rounded square (a group, an agent). */
@Composable
fun Avatar(text: String, size: Dp = 46.dp, square: Boolean = false) {
    val initials = text.split(' ', '-', '_').filter { it.isNotEmpty() }.take(2)
        .joinToString("") { it.take(1).uppercase() }.ifEmpty { "?" }
    val (bg, fg) = if (square) C.AvatarBg to C.AvatarText else C.tint(text)
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
                textStyle = TextStyle(fontFamily = Plex, fontSize = 16.sp, color = C.Text),
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

/** A rounded surface card with a hairline border. */
@Composable
fun CardBox(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)).background(C.Surface)
            .border(1.dp, C.Border, RoundedCornerShape(18.dp)).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) { content() }
}

/** The title row used by every sub-screen: a 44dp back button and the title. */
@Composable
fun TopBar(title: String, onBack: (() -> Unit)?, trailing: @Composable () -> Unit = {}) {
    Row(
        Modifier.fillMaxWidth().background(C.Surface).statusBarsPadding().padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (onBack != null) {
            Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onBack).semantics { contentDescription = "Back" }, contentAlignment = Alignment.Center) {
                Ico(Ic.Back)
            }
        }
        Text(title, fontFamily = Grotesk, fontWeight = FontWeight.SemiBold, fontSize = 20.sp, modifier = Modifier.weight(1f), maxLines = 1)
        trailing()
    }
    Box(Modifier.fillMaxWidth().size(1.dp).background(C.Border))
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
