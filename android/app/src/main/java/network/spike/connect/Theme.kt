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

/** The prototype's palette (docs/mobile-prototype): colour means status only. */
object C {
    val Bg = Color(0xFFF5F7F9)
    val BgChat = Color(0xFFEEF2F5)
    val Surface = Color(0xFFFFFFFF)
    val Border = Color(0xFFD9E0E6)
    val BorderStrong = Color(0xFFC5CFD8)
    val Text = Color(0xFF14202B)
    val Muted = Color(0xFF52606D)
    val AvatarBg = Color(0xFFE6EBF0)
    val AvatarText = Color(0xFF3A4753)
    val Accent = Color(0xFF0B6B73)
    val Green = Color(0xFF1F7A45)
    val AmberText = Color(0xFF7A4A00)
    val AmberDeep = Color(0xFF4A3200)
    val AmberBg = Color(0xFFFBEED0)
    val AmberBorder = Color(0xFFE4C98F)
    val RedText = Color(0xFF6E1F1C)
    val RedBg = Color(0xFFF8E1E1)
    val Danger = Color(0xFF8E2B27)
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
fun SpikeTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = lightColorScheme(primary = C.Accent, background = C.Bg, surface = C.Surface, onSurface = C.Text),
    ) {
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

/** Initials in a neutral circle (a person) or rounded square (a group, an agent). */
@Composable
fun Avatar(text: String, size: Dp = 46.dp, square: Boolean = false) {
    val initials = text.split(' ', '-', '_').filter { it.isNotEmpty() }.take(2)
        .joinToString("") { it.take(1).uppercase() }.ifEmpty { "?" }
    Box(
        Modifier.size(size).clip(if (square) RoundedCornerShape(14.dp) else CircleShape).background(C.AvatarBg),
        contentAlignment = Alignment.Center,
    ) {
        Text(initials, color = C.AvatarText, fontWeight = FontWeight.SemiBold, fontSize = (size.value * 0.33f).sp)
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
        text, color = fg, fontSize = 11.sp,
        modifier = Modifier.clip(RoundedCornerShape(4.dp)).background(bg).padding(horizontal = 6.dp, vertical = 1.dp),
    )
}

/** A rounded pill with an optional dot or icon, like the status chips on the chat list. */
@Composable
fun Chip(text: String, dot: Color? = null, icon: ImageVector? = null, onClick: () -> Unit = {}) {
    Row(
        Modifier.heightIn(min = 32.dp).clip(RoundedCornerShape(999.dp)).background(C.Surface)
            .border(1.dp, C.Border, RoundedCornerShape(999.dp)).clickable(onClick = onClick)
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
        if (icon != null) Ico(icon, Color.White, 20.dp)
        Text(text, color = Color.White, fontWeight = FontWeight.SemiBold, fontSize = 16.sp)
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

/** A white rounded card with the prototype's 1px border. */
@Composable
fun CardBox(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(C.Surface)
            .border(1.dp, C.Border, RoundedCornerShape(14.dp)).padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
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
        Ico(icon, C.AvatarText, 24.dp)
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
