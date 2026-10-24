package network.spike.connect

/** The Rust library (`mobile/`): open an engine on a directory, run JSON commands on it, close it. */
object Native {
    init {
        System.loadLibrary("spike_mobile")
    }

    @JvmStatic external fun open(dir: String): Long

    @JvmStatic external fun call(handle: Long, command: String, args: String): String

    @JvmStatic external fun close(handle: Long)
}
