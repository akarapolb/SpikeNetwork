package network.spike.connect

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/** The prototype's line icons (docs/mobile-prototype), 24x24 with a 2px round stroke. Generated from its SVGs. */
object Ic {
    private fun icon(name: String, vararg paths: String): ImageVector {
        val b = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        for (p in paths) b.addPath(
            pathData = addPathNodes(p), fill = null, stroke = SolidColor(Color.Black), strokeLineWidth = 2f,
            strokeLineCap = StrokeCap.Round, strokeLineJoin = StrokeJoin.Round,
        )
        return b.build()
    }

    val Back = icon("Back", "M15 5l-7 7 7 7")
    val Chevron = icon("Chevron", "M9 5l7 7-7 7")
    val Link = icon("Link", "M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7", "M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7")
    val Chat = icon("Chat", "M4 5h16v11H9l-5 4z")
    val Sparkle = icon("Sparkle", "M12 3l1.8 4.6L18.5 9l-4.7 1.4L12 15l-1.8-4.6L5.5 9l4.7-1.4z")
    val Devices = icon("Devices", "M4.5 5h9a1.5 1.5 0 0 1 1.5 1.5v6a1.5 1.5 0 0 1 -1.5 1.5h-9a1.5 1.5 0 0 1 -1.5 -1.5v-6a1.5 1.5 0 0 1 1.5 -1.5z", "M1.5 17h15", "M18.2 8h3.1a1.2 1.2 0 0 1 1.2 1.2v9.6a1.2 1.2 0 0 1 -1.2 1.2h-3.1a1.2 1.2 0 0 1 -1.2 -1.2v-9.6a1.2 1.2 0 0 1 1.2 -1.2z")
    val Sliders = icon("Sliders", "M4 7h10M18 7h2M4 17h4M12 17h8", "M14 7a2 2 0 1 0 4 0a2 2 0 1 0 -4 0z", "M8 17a2 2 0 1 0 4 0a2 2 0 1 0 -4 0z")
    val ShieldCheck = icon("ShieldCheck", "M12 3l7 3v6c0 4-3 7-7 9-4-2-7-5-7-9V6z", "M9 12l2 2 4-4")
    val Info = icon("Info", "M3 12a9 9 0 1 0 18 0a9 9 0 1 0 -18 0z", "M12 11v6M12 7.5v.5")
    val Phone = icon("Phone", "M8 3h8a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-8a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2z", "M11 17h2")
    val Video = icon("Video", "M5 5h9a2 2 0 0 1 2 2v10a2 2 0 0 1 -2 2h-9a2 2 0 0 1 -2 -2v-10a2 2 0 0 1 2 -2z", "M16 10l5-3v10l-5-3")
    val File = icon("File", "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z", "M14 3v5h5")
    val Clip = icon("Clip", "M21 12l-8.5 8.5a5 5 0 0 1-7-7L14 5a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 8")
    val Send = icon("Send", "M5 12h13M13 6l6 6-6 6")
    val PhoneDevice = icon("PhoneDevice", "M8.5 2.5h7a2.5 2.5 0 0 1 2.5 2.5v14a2.5 2.5 0 0 1 -2.5 2.5h-7a2.5 2.5 0 0 1 -2.5 -2.5v-14a2.5 2.5 0 0 1 2.5 -2.5z", "M11 18h2")
    val Laptop = icon("Laptop", "M4.5 4.5h15a1.5 1.5 0 0 1 1.5 1.5v9a1.5 1.5 0 0 1 -1.5 1.5h-15a1.5 1.5 0 0 1 -1.5 -1.5v-9a1.5 1.5 0 0 1 1.5 -1.5z", "M1.5 19.5h21")
    val Tablet = icon("Tablet", "M6 2.5h12a2 2 0 0 1 2 2v15a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-15a2 2 0 0 1 2 -2z", "M11 18.5h2")
    val Plus = icon("Plus", "M12 5v14M5 12h14")
    val Check = icon("Check", "M5 12l5 5 9-10")
    val AddPerson = icon("AddPerson", "M5.5 8a3.5 3.5 0 1 0 7 0a3.5 3.5 0 1 0 -7 0z", "M2.5 20c.8-3.5 3.4-5.5 6.5-5.5s5.7 2 6.5 5.5", "M19 8v6M16 11h6")
    val Lock = icon("Lock", "M7 11h10a2 2 0 0 1 2 2v6a2 2 0 0 1 -2 2h-10a2 2 0 0 1 -2 -2v-6a2 2 0 0 1 2 -2z", "M8 11V7a4 4 0 0 1 8 0v4")
    val Search = icon("Search", "M4 11a7 7 0 1 0 14 0a7 7 0 1 0 -14 0z", "M20 20l-3.5-3.5")
    val Request = icon("Request", "M4 4h16v12H8l-4 4z", "M12 7v4M12 13.5v.5")
    val Assistant = icon("Assistant", "M12 3l1.8 4.6L18.5 9l-4.7 1.4L12 15l-1.8-4.6L5.5 9l4.7-1.4z", "M19 15l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8z")
    val Bookmark = icon("Bookmark", "M5 4h14v16l-7-4-7 4z")
    val Server = icon("Server", "M4.5 4h15a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1 -1.5 1.5h-15a1.5 1.5 0 0 1 -1.5 -1.5v-4a1.5 1.5 0 0 1 1.5 -1.5z", "M4.5 13h15a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1 -1.5 1.5h-15a1.5 1.5 0 0 1 -1.5 -1.5v-4a1.5 1.5 0 0 1 1.5 -1.5z", "M7 7.5h.01M7 16.5h.01")
    val Share = icon("Share", "M3.5 12a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0z", "M15.5 6a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0z", "M15.5 18a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0z", "M8.3 11l7.4-3.8M8.3 13l7.4 3.8")
    val Scan = icon("Scan", "M4 8V5.5A1.5 1.5 0 0 1 5.5 4H8M16 4h2.5A1.5 1.5 0 0 1 20 5.5V8M20 16v2.5a1.5 1.5 0 0 1-1.5 1.5H16M8 20H5.5A1.5 1.5 0 0 1 4 18.5V16M4 12h16")
    val Person = icon("Person", "M8 8a4 4 0 1 0 8 0a4 4 0 1 0 -8 0z", "M4 21c1-4 4.5-6 8-6s7 2 8 6")
}
