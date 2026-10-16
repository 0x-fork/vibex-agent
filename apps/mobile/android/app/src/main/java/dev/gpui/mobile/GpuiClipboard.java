package dev.gpui.mobile;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;

/**
 * The system clipboard helper {@code gpui-pre-mobile} looks up by name.
 *
 * <p>{@code gpui_mobile::packages::clipboard} reaches the Android clipboard
 * through {@code ClassLoader.loadClass("dev.gpui.mobile.GpuiClipboard")} on the
 * host Activity, so the class has to be packaged by the application, not by the
 * platform layer. Without it every {@code write_to_clipboard} silently falls
 * back to the platform layer's in-process string store: the text can be pasted
 * back inside Vibex, but no other application ever sees it.
 *
 * <p>Adapted from the {@code gpui-mobile} example's
 * {@code dev.gpui.mobile.GpuiClipboard} (AGPL-3.0-or-later, rev 4e4668d). The
 * package name is fixed by the JNI lookup, so this class keeps it.
 */
public final class GpuiClipboard {

    /**
     * Puts {@code text} on the system clipboard, replacing whatever was there.
     *
     * <p>Android 13 and later draw their own "copied" confirmation for this;
     * Vibex must not draw a second one.
     */
    public static void setText(Activity activity, String text) {
        ClipboardManager manager =
                (ClipboardManager) activity.getSystemService(Context.CLIPBOARD_SERVICE);
        if (manager == null) {
            return;
        }
        manager.setPrimaryClip(ClipData.newPlainText("Vibex", text));
    }

    /**
     * Returns the clipboard's text, or {@code null} when it holds none.
     *
     * <p>Android only answers this for the application the user is looking at,
     * which is the only time Vibex reads it.
     */
    public static String getText(Activity activity) {
        ClipData clip = primaryClip(activity);
        if (clip == null || clip.getItemCount() == 0) {
            return null;
        }
        CharSequence text = clip.getItemAt(0).getText();
        return text == null ? null : text.toString();
    }

    /** Whether the clipboard currently holds text. */
    public static boolean hasText(Activity activity) {
        return getText(activity) != null;
    }

    private static ClipData primaryClip(Activity activity) {
        ClipboardManager manager =
                (ClipboardManager) activity.getSystemService(Context.CLIPBOARD_SERVICE);
        if (manager == null || !manager.hasPrimaryClip()) {
            return null;
        }
        return manager.getPrimaryClip();
    }

    private GpuiClipboard() {}
}
