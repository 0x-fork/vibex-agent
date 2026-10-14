package ai.vibex.mobile;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Rect;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.util.Base64;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.MediaStore;
import android.provider.Settings;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.text.TextWatcher;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputConnectionWrapper;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;

import androidx.core.content.FileProvider;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.Collections;
import java.util.HashMap;
import java.util.Map;

/**
 * Vibex's Android host.
 *
 * A plain {@link Activity} that owns a {@link SurfaceView} and hands everything
 * the GPUI host entry needs — the surface, touch and key events, the IME, window
 * insets and the lifecycle — to {@code vibex_mobile}'s
 * {@code Java_ai_vibex_mobile_GpuiHostActivity_*} entry points.
 *
 * <p>The native side runs a process-lived render thread, so this Activity can be
 * destroyed and recreated freely: a new instance re-attaches its surface to the
 * same GPUI window instead of trying to build a second platform. That is what
 * keeps a backgrounded app usable after Android reclaims its Activity while the
 * foreground connection service keeps the process alive.
 *
 * <p>Everything Vibex-specific — notifications, LAN discovery, the foreground
 * service, the battery allowlist — lives here too, and is reached from Rust by
 * method name through the current Activity.
 */
public final class GpuiHostActivity extends Activity {
    private static final int LOCAL_NETWORK_PERMISSION_REQUEST = 4102;
    private static final int NOTIFICATION_PERMISSION_REQUEST = 4103;
    private static final int PICK_IMAGE_REQUEST = 4104;
    private static final int PICK_FILE_REQUEST = 4105;
    private static final int CAPTURE_PHOTO_REQUEST = 4106;
    /** The longest side a picked image is scaled down to before it is encoded. */
    private static final int ATTACHMENT_MAX_EDGE = 2048;
    /** The encoded attachment budget; a phone photograph has to fit the relay. */
    private static final int ATTACHMENT_MAX_BYTES = 6 * 1024 * 1024;
    /**
     * The budget for a picked file, which is carried whole rather than scaled.
     * It is the same wire the relay already carries a photograph over.
     */
    private static final int FILE_ATTACHMENT_MAX_BYTES = 8 * 1024 * 1024;
    private static final String AGENT_NOTIFICATION_CHANNEL = "agent_activity";
    private static final String UPDATE_NOTIFICATION_CHANNEL = "app_updates";
    private static final String UPDATE_NOTIFICATION_TAG = "vibex_update";
    private static final int UPDATE_NOTIFICATION_ID = 0x564259;
    private static final String ACTION_OPEN_APP_UPDATE = "ai.vibex.mobile.OPEN_APP_UPDATE";
    private static final String EXTRA_NOTIFICATION_ID = "vibex.notification.id";
    private static final String EXTRA_NOTIFICATION_LOCATOR = "vibex.notification.locator";
    private static final String NOTIFICATION_PERMISSION_PREFERENCES = "vibex.notifications";
    private static final String NOTIFICATION_PERMISSION_REQUESTED = "permission_requested";
    private static final String VIBEX_SERVICE_TYPE = "_vibex._tcp.";
    private static final String UPDATE_FILE_AUTHORITY_SUFFIX = ".updates";
    private static final String CAPTURE_FILE_AUTHORITY_SUFFIX = ".captures";
    /** The action a launcher shortcut uses to open a fresh session. */
    private static final String ACTION_NEW_SESSION = "ai.vibex.mobile.NEW_SESSION";
    /**
     * Width of the strip along the left edge the system's own back gesture may
     * not claim, in density-independent pixels.
     *
     * <p>The sessions drawer is dragged open from that edge, and a swipe the
     * system claims never reaches GPUI: it arrives as BACK instead, so the
     * drawer stays shut and the swipe does nothing. The app never leaves the
     * foreground for it, but the gesture still has to be handed back to GPUI to
     * open the drawer at all. The platform's own gesture inset is used where it
     * reports one; this is the floor for the devices that do not.
     */
    private static final int DRAWER_EDGE_DP = 24;
    /**
     * The host id every native call carries.
     *
     * <p>{@code gpui-pre-mobile} keeps one GPUI window per host id and builds it
     * through {@code on_open_window} the first time that id's surface arrives.
     * Vibex shows a single fullscreen panel, so it names one constant id: a
     * recreated Activity re-attaches the surface to the window it left instead
     * of opening a second one, and the panel keeps its state.
     *
     * <p>The id is never retired through {@code host_destroyed}. The foreground
     * connection service keeps the process alive after the Activity finishes, so
     * holding the window is what lets a later launch return to the panel as the
     * user left it rather than rebuilding it from scratch.
     */
    private static final long HOST_ID = 1L;

    static {
        // A plain Activity has no `android.app.lib_name` meta-data to load the
        // native library, so the host loads the JNI entry points itself.
        System.loadLibrary("vibex_mobile");
    }

    private SurfaceView surfaceView;
    private InputProxy input;
    /** Width of the left-edge strip kept out of the system's back gesture. */
    private int drawerEdgeWidthPx;
    /** The strip already handed to the framework, so an unchanged one is not re-sent. */
    private int appliedDrawerEdgeWidthPx;
    private int appliedDrawerEdgeHeightPx;
    /** The file a pending camera capture writes into, cleared once it reports. */
    private File captureTarget;

    private NsdManager nsdManager;
    private NsdManager.DiscoveryListener lanDiscoveryListener;
    private final Map<String, NsdManager.ResolveListener> pendingResolutions = new HashMap<>();

    private static native void nativeStart(Activity activity, String dataDir);
    private static native void nativeSurfaceCreated(long hostId, Surface surface, float scale);
    private static native void nativeSurfaceDestroyed(long hostId);
    private static native void nativeOnAppLifecycle(long hostId, boolean foreground);
    private static native void nativeInsets(int left, int top, int right, int bottom);
    private static native void nativeKeyboardState(boolean visible, float height);
    private static native void nativeIme(long session, int kind, String text, int start, int end);
    private static native void nativeTouch(int action, int pointerId, float x, float y);
    private static native void nativeKey(int keyCode, int action, int metaState);
    private static native void nativeOnLanDiscoveryEvent(String payload);
    private static native void nativeOnNotificationActivated(
            String notificationId, String opaqueLocator);
    private static native void nativeOnUpdateNotificationActivated();
    private static native void nativeOnAttachmentPicked(String label, String mimeType, String dataUrl);
    private static native void nativeOnAttachmentPickFailed(String message);
    /** Text shared into Vibex from another app, which lands in the composer. */
    private static native void nativeOnSharedText(String text);
    /** A launcher shortcut was used: open a new session instead of restoring. */
    private static native void nativeOnLaunchShortcut(String shortcut);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // GPUI draws edge to edge and pads its own root view by the insets
        // reported through `nativeInsets`, so the window must not inset the
        // surface the system bars leave behind.
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);

        surfaceView = new SurfaceView(this);
        drawerEdgeWidthPx = Math.round(
                DRAWER_EDGE_DP * getResources().getDisplayMetrics().density);
        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                attachSurface(holder);
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
                attachSurface(holder);
                reportCurrentInsets();
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                nativeSurfaceDestroyed(HOST_ID);
            }
        });
        setContentView(surfaceView);

        View decor = getWindow().getDecorView();
        // The excluded strip spans the whole left edge, so its height follows
        // the window: every layout re-issues it, and `reportInsets` widens it to
        // whatever the platform calls its own gesture area.
        decor.addOnLayoutChangeListener(
                (view, left, top, right, bottom, oldLeft, oldTop, oldRight, oldBottom) ->
                        applyDrawerEdgeExclusion());
        ViewCompat.setOnApplyWindowInsetsListener(decor, (view, insets) -> {
            reportInsets(insets);
            return insets;
        });

        createAgentNotificationChannel();
        createUpdateNotificationChannel();
        handleNotificationIntent(getIntent());
        handleIncomingIntent(getIntent());
        nativeStart(this, getFilesDir().getAbsolutePath());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleNotificationIntent(intent);
        handleIncomingIntent(intent);
    }

    /**
     * Routes the two intents that are not notification taps: content another
     * app shared in, and a launcher shortcut.
     *
     * The intent is consumed here so a configuration change or a return to the
     * task does not deliver the same shared picture twice.
     */
    private void handleIncomingIntent(Intent intent) {
        if (intent == null) {
            return;
        }
        String shortcut = intent.getAction();
        if (ACTION_NEW_SESSION.equals(shortcut)) {
            intent.setAction(null);
            nativeOnLaunchShortcut("new_session");
            return;
        }
        if (!Intent.ACTION_SEND.equals(shortcut)) {
            return;
        }
        String type = intent.getType();
        intent.setAction(null);
        if (type != null && type.startsWith("image/")) {
            Uri uri = intent.getParcelableExtra(Intent.EXTRA_STREAM);
            if (uri != null) {
                reportAttachment(uri, true, "shared-image");
            }
            return;
        }
        String text = intent.getStringExtra(Intent.EXTRA_TEXT);
        if (text != null && !text.isBlank()) {
            nativeOnSharedText(text);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        nativeOnAppLifecycle(HOST_ID, true);
    }

    @Override
    protected void onPause() {
        nativeOnAppLifecycle(HOST_ID, false);
        super.onPause();
    }

    /**
     * Opens the system picker for one image to attach to the composer.
     *
     * The document picker grants read access to the single picked item, so this
     * needs no storage permission. The bytes never leave the phone as a file the
     * desktop cannot see: they are scaled down and encoded as a data URL, which
     * the desktop materializes into its own attachment directory.
     */
    public void gpuiPickImageAttachment() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("image/*");
        try {
            startActivityForResult(intent, PICK_IMAGE_REQUEST);
        } catch (ActivityNotFoundException error) {
            nativeOnAttachmentPickFailed("No file picker is available on this device");
        }
    }

    /**
     * Opens the system picker for any one file to attach to the composer.
     *
     * The image path scales a photograph down; this one carries the bytes as
     * they are, so a log, a patch, or a document keeps its own format. The
     * budget is the file's whole size, which is what the relay has to carry.
     */
    public void gpuiPickFileAttachment() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        try {
            startActivityForResult(intent, PICK_FILE_REQUEST);
        } catch (ActivityNotFoundException error) {
            nativeOnAttachmentPickFailed("No file picker is available on this device");
        }
    }

    /**
     * Opens the system camera and attaches the photograph it takes.
     *
     * The capture is written to a file this app owns, so the result needs no
     * storage permission and no media scan; it is scaled and encoded by the
     * same path a picked image takes.
     */
    public void gpuiCapturePhotoAttachment() {
        Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);
        if (intent.resolveActivity(getPackageManager()) == null) {
            nativeOnAttachmentPickFailed("No camera app is available on this device");
            return;
        }
        try {
            File directory = new File(getCacheDir(), "captures");
            if (!directory.exists() && !directory.mkdirs()) {
                throw new IOException("the capture directory could not be created");
            }
            File target = File.createTempFile("capture-", ".jpg", directory);
            captureTarget = target;
            Uri output = FileProvider.getUriForFile(
                    this,
                    getPackageName() + CAPTURE_FILE_AUTHORITY_SUFFIX,
                    target);
            intent.putExtra(MediaStore.EXTRA_OUTPUT, output);
            intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            startActivityForResult(intent, CAPTURE_PHOTO_REQUEST);
        } catch (Exception error) {
            captureTarget = null;
            nativeOnAttachmentPickFailed("The camera could not be opened");
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == CAPTURE_PHOTO_REQUEST) {
            File captured = captureTarget;
            captureTarget = null;
            if (resultCode != RESULT_OK || captured == null || !captured.exists()) {
                // A cancelled capture is not a failure.
                return;
            }
            reportAttachment(Uri.fromFile(captured), true, "photo");
            return;
        }
        boolean image = requestCode == PICK_IMAGE_REQUEST;
        if (!image && requestCode != PICK_FILE_REQUEST) {
            return;
        }
        Uri uri = (resultCode == RESULT_OK && data != null) ? data.getData() : null;
        if (uri == null) {
            // A cancelled pick is not a failure: nothing is reported, and the
            // composer simply keeps the attachments it already had.
            return;
        }
        reportAttachment(uri, image, image ? "image" : "file");
    }

    /**
     * Reads, scales, and encodes one item, then reports it to the panel.
     *
     * Decoding and encoding a camera-sized image takes long enough to drop
     * frames, so it happens off the UI thread and reports back through the
     * Activity's own thread.
     */
    private void reportAttachment(Uri uri, boolean image, String fallbackLabel) {
        new Thread(() -> {
            String label = attachmentLabel(uri, fallbackLabel);
            try {
                String mimeType = getContentResolver().getType(uri);
                byte[] encoded = image ? encodeAttachment(uri) : readAttachment(uri);
                if (mimeType == null || mimeType.isBlank()) {
                    mimeType = image ? "image/jpeg" : "application/octet-stream";
                } else if (image && !mimeType.startsWith("image/")) {
                    mimeType = "image/jpeg";
                }
                String dataUrl = "data:" + mimeType + ";base64,"
                        + Base64.encodeToString(encoded, Base64.NO_WRAP);
                String finalLabel = label;
                String finalMimeType = mimeType;
                runOnUiThread(() -> nativeOnAttachmentPicked(finalLabel, finalMimeType, dataUrl));
            } catch (Exception error) {
                runOnUiThread(() -> nativeOnAttachmentPickFailed(image
                        ? "The picked image could not be read"
                        : "The picked file could not be read"));
            }
        }).start();
    }

    /**
     * Holds the display awake while the panel is watching work run.
     *
     * <p>Android's idle timer is the only thing that turns the screen off while
     * the app is in front of the user. The flag is a property of the window, so
     * it survives configuration changes and is cleared when the panel stops
     * asking for it — a phone left alone in the app still sleeps.
     */
    public void gpuiKeepScreenOn(boolean keepOn) {
        runOnUiThread(() -> {
            if (keepOn) {
                getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            } else {
                getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            }
        });
    }

    /**
     * Hands text to whichever app the user picks.
     *
     * The mirror of the share intent-filter: a phone's share sheet is the way
     * a transcript leaves the app, and the desktop has no equivalent.
     */
    public void gpuiShareText(String text) {
        runOnUiThread(() -> {
            if (text == null || text.isBlank()) {
                return;
            }
            Intent send = new Intent(Intent.ACTION_SEND);
            send.setType("text/plain");
            send.putExtra(Intent.EXTRA_TEXT, text);
            Intent chooser = Intent.createChooser(send, "Share");
            chooser.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                startActivity(chooser);
            } catch (ActivityNotFoundException error) {
                // Nothing to share with; the copy button is still there.
            }
        });
    }

    /** Reads one picked file whole, up to the wire budget. */
    private byte[] readAttachment(Uri uri) throws Exception {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[64 * 1024];
        long total = 0;
        try (InputStream stream = getContentResolver().openInputStream(uri)) {
            if (stream == null) {
                throw new IllegalStateException("the picked file cannot be opened");
            }
            int read;
            while ((read = stream.read(buffer)) > 0) {
                total += read;
                if (total > FILE_ATTACHMENT_MAX_BYTES) {
                    throw new IllegalStateException("the picked file is too large to attach");
                }
                out.write(buffer, 0, read);
            }
        }
        return out.toByteArray();
    }

    /** Reads one picked image, scaled down to what a relayed prompt can carry. */
    private byte[] encodeAttachment(Uri uri) throws Exception {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        try (InputStream stream = getContentResolver().openInputStream(uri)) {
            BitmapFactory.decodeStream(stream, null, bounds);
        }
        BitmapFactory.Options decode = new BitmapFactory.Options();
        int longestEdge = Math.max(bounds.outWidth, bounds.outHeight);
        int sampleSize = 1;
        while (longestEdge > 0 && longestEdge / (sampleSize * 2) >= ATTACHMENT_MAX_EDGE) {
            sampleSize *= 2;
        }
        decode.inSampleSize = sampleSize;
        Bitmap bitmap;
        try (InputStream stream = getContentResolver().openInputStream(uri)) {
            bitmap = BitmapFactory.decodeStream(stream, null, decode);
        }
        if (bitmap == null) {
            throw new IllegalStateException("the picked image has no decodable pixels");
        }
        String mimeType = getContentResolver().getType(uri);
        boolean keepAlpha = mimeType != null && mimeType.contains("png");
        Bitmap.CompressFormat format =
                keepAlpha ? Bitmap.CompressFormat.PNG : Bitmap.CompressFormat.JPEG;
        byte[] encoded = compress(bitmap, format, keepAlpha ? 100 : 88);
        // Still too large for the wire: halve the pixels once and try again.
        if (encoded.length > ATTACHMENT_MAX_BYTES) {
            Bitmap scaled = Bitmap.createScaledBitmap(
                    bitmap,
                    Math.max(1, bitmap.getWidth() / 2),
                    Math.max(1, bitmap.getHeight() / 2),
                    true);
            encoded = compress(scaled, Bitmap.CompressFormat.JPEG, 80);
        }
        bitmap.recycle();
        if (encoded.length > ATTACHMENT_MAX_BYTES * 3 / 2) {
            throw new IllegalStateException("the picked image is too large to attach");
        }
        return encoded;
    }

    private static byte[] compress(Bitmap bitmap, Bitmap.CompressFormat format, int quality) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        bitmap.compress(format, quality, out);
        return out.toByteArray();
    }

    /** The name the reader sees on the attachment chip. */
    private String attachmentLabel(Uri uri, String fallback) {
        String displayName = null;
        try (android.database.Cursor cursor = getContentResolver().query(
                uri, new String[]{android.provider.OpenableColumns.DISPLAY_NAME},
                null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int index = cursor.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME);
                if (index >= 0) {
                    displayName = cursor.getString(index);
                }
            }
        } catch (Exception ignored) {
            // A provider that does not answer the metadata query still has a
            // last path segment worth showing.
        }
        if (displayName == null || displayName.isBlank()) {
            String lastSegment = uri.getLastPathSegment();
            displayName = (lastSegment == null || lastSegment.isBlank())
                    ? fallback
                    : lastSegment;
        }
        return displayName;
    }

    /** Hands the surface Android just created to the render thread. */
    private void attachSurface(SurfaceHolder holder) {
        Surface surface = holder.getSurface();
        if (surface == null || !surface.isValid()) {
            return;
        }
        nativeSurfaceCreated(HOST_ID, surface, getResources().getDisplayMetrics().density);
    }

    /** Reports the window insets GPUI has to lay out around. */
    private void reportCurrentInsets() {
        WindowInsetsCompat insets = ViewCompat.getRootWindowInsets(getWindow().getDecorView());
        if (insets != null) {
            reportInsets(insets);
        }
    }

    /**
     * Keeps the system's own back gesture out of the strip the sessions drawer
     * is dragged open from.
     *
     * <p>{@code setSystemGestureExclusionRects} is the only way to hand an edge
     * swipe back to the app once gesture navigation owns that edge, and the
     * system caps what an app may exclude, so this asks for the drawer's own
     * edge and nothing more. A platform that reports no gesture area still gets
     * the floor, because the swipe has to reach GPUI either way.
     */
    private void excludeDrawerEdgeFromSystemGesture(WindowInsetsCompat insets) {
        int floor = Math.round(DRAWER_EDGE_DP * getResources().getDisplayMetrics().density);
        Insets gestures = insets.getInsets(WindowInsetsCompat.Type.systemGestures());
        drawerEdgeWidthPx = Math.max(gestures.left, floor);
        applyDrawerEdgeExclusion();
    }

    /**
     * Re-issues the excluded left-edge strip for the surface's current size.
     *
     * <p>It does nothing below API 29, where the framework has no such knob and
     * the edge belongs to the app alone. Called again on every layout and inset
     * change, because the rect spans the window's whole height; an unchanged
     * strip is not re-sent.
     */
    private void applyDrawerEdgeExclusion() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            return;
        }
        // The rects go on the decor, never on the SurfaceView: the surface's
        // content belongs to another process, and the framework takes exclusion
        // rects from the window's own hierarchy. A rect that never reaches the
        // window is a swipe the system keeps claiming.
        View decor = getWindow().getDecorView();
        int height = decor.getHeight();
        if (drawerEdgeWidthPx <= 0 || height <= 0) {
            return;
        }
        if (drawerEdgeWidthPx == appliedDrawerEdgeWidthPx && height == appliedDrawerEdgeHeightPx) {
            return;
        }
        decor.setSystemGestureExclusionRects(
                Collections.singletonList(new Rect(0, 0, drawerEdgeWidthPx, height)));
        appliedDrawerEdgeWidthPx = drawerEdgeWidthPx;
        appliedDrawerEdgeHeightPx = height;
    }

    private void reportInsets(WindowInsetsCompat insets) {
        Insets bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars()
                        | WindowInsetsCompat.Type.displayCutout());
        // The GPUI render thread may not have processed surfaceChanged yet.
        // Send edge distances, never a rect derived from this View's new size.
        nativeInsets(bars.left, bars.top, bars.right, bars.bottom);
        excludeDrawerEdgeFromSystemGesture(insets);

        boolean visible = insets.isVisible(WindowInsetsCompat.Type.ime());
        Insets ime = insets.getInsets(WindowInsetsCompat.Type.ime());
        // `adjustResize` still shrinks the window for the keyboard below API 30,
        // so the surface already excludes it there; from API 30 on the app is
        // edge to edge and this inset is the only thing that lifts the composer
        // above the keyboard. The height GPUI wants is in logical pixels.
        boolean heightInLayout = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R;
        float density = getResources().getDisplayMetrics().density;
        nativeKeyboardState(visible, visible && heightInLayout ? ime.bottom / density : 0f);
    }

    @Override
    public boolean dispatchTouchEvent(MotionEvent event) {
        int action = event.getActionMasked();
        int index = event.getActionIndex();
        if (action == MotionEvent.ACTION_POINTER_DOWN || action == MotionEvent.ACTION_POINTER_UP) {
            // `motion_event` collapses a pointer change onto the affected
            // pointer, so only that one is forwarded.
            nativeTouch(action, event.getPointerId(index), event.getX(index), event.getY(index));
            return true;
        }
        for (int i = 0; i < event.getPointerCount(); i++) {
            nativeTouch(action, event.getPointerId(i), event.getX(i), event.getY(i));
        }
        return true;
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        // While the software keyboard is up the IME proxy owns the keys: it
        // reports BACK as a dismissed IME, which is the same thing GPUI would
        // otherwise do on its own.
        if (input != null && input.hasFocus() && super.dispatchKeyEvent(event)) {
            return true;
        }
        int action = event.getAction();
        if (action == KeyEvent.ACTION_DOWN || action == KeyEvent.ACTION_UP) {
            nativeKey(
                    event.getKeyCode(),
                    action == KeyEvent.ACTION_DOWN ? 0 : 1,
                    event.getMetaState());
            // Every key is consumed so GPUI sees it, BACK included. Nothing
            // here turns BACK into leaving the app: the back gesture is the
            // same rightward edge swipe the sessions drawer opens with, so the
            // Rust handler keeps the app in the foreground when it runs out of
            // screens to close.
        }
        return true;
    }

    // ── software keyboard ────────────────────────────────────────────────────
    //
    // The InputConnection bridge below is adapted from the gpui-mobile example's
    // `dev.gpui.mobile.GpuiInputActivity` (AGPL-3.0-or-later, rev 4e4668d). It
    // used to be vendored as a `NativeActivity` base class; the host entry owns
    // its Activity instead, so the same proxy lives here and reports through the
    // `nativeIme` entry point for this class.

    /**
     * Shows the UI-thread InputConnection proxy the IME writes into.
     *
     * A {@code NativeActivity} key-event connection cannot compose text or
     * delete surrounding text; the proxy's {@link InputConnectionWrapper} turns
     * every IME mutation into a {@code nativeIme} callback that GPUI applies to
     * the focused input.
     */
    public void gpuiShowKeyboard(int keyboardType, long session) {
        runOnUiThread(() -> {
            if (input == null) {
                input = new InputProxy();
                input.setAlpha(0f);
                input.setPadding(0, 0, 0, 0);
                addContentView(input, new ViewGroup.LayoutParams(1, 1));
            }
            input.reset(session);
            int type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            switch (keyboardType) {
                case 1: type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS; break;
                case 2: type = InputType.TYPE_CLASS_PHONE; break;
                case 3: type = InputType.TYPE_CLASS_NUMBER; break;
                case 4: type = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI; break;
                case 5: type = InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL; break;
            }
            input.setInputType(type);
            input.setImeOptions(EditorInfo.IME_FLAG_NO_EXTRACT_UI);
            input.requestFocus();
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            imm.restartInput(input);
            imm.showSoftInput(input, InputMethodManager.SHOW_IMPLICIT);
        });
    }

    public void gpuiHideKeyboard(long session) {
        runOnUiThread(() -> {
            if (input == null) return;
            input.reset(session);
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            imm.hideSoftInputFromWindow(input.getWindowToken(), 0);
            input.clearFocus();
        });
    }

    public void gpuiResetComposition(long session) {
        runOnUiThread(() -> {
            if (input == null) return;
            input.reset(session);
            ((InputMethodManager) getSystemService(INPUT_METHOD_SERVICE)).restartInput(input);
        });
    }

    /** The InputConnection host GPUI drives through {@code nativeIme}. */
    private final class InputProxy extends EditText {
        private long session;
        private int depth;
        private boolean marked;

        InputProxy() {
            super(GpuiHostActivity.this);
            addTextChangedListener(new TextWatcher() {
                public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
                public void onTextChanged(CharSequence s, int start, int before, int count) {}
                public void afterTextChanged(Editable text) {
                    // Hardware keyboards edit the widget directly, outside its
                    // InputConnection. IME mutations are batched by depth below.
                    if (depth == 0) { depth++; endEdit(); }
                }
            });
        }

        @Override
        public boolean onKeyDown(int code, KeyEvent event) {
            if (code == KeyEvent.KEYCODE_DEL && getText().length() == 0 && !marked) {
                nativeIme(session, 3, "", 1, 0);
                return true;
            }
            return super.onKeyDown(code, event);
        }

        void reset(long nextSession) {
            depth++;
            getText().clear();
            marked = false;
            session = nextSession;
            depth = 0;
        }

        private void endEdit() {
            if (--depth != 0) return;
            Editable text = getText();
            boolean composing = BaseInputConnection.getComposingSpanStart(text) >= 0;
            if (composing || marked || text.length() > 0) {
                nativeIme(session, composing ? 0 : 1, text.toString(),
                        Math.max(0, Selection.getSelectionStart(text)),
                        Math.max(0, Selection.getSelectionEnd(text)));
                marked = composing;
                if (!composing) {
                    depth++;
                    text.clear();
                    depth--;
                }
            }
        }

        @Override
        public boolean onKeyPreIme(int code, KeyEvent event) {
            if (code == KeyEvent.KEYCODE_BACK && event.getAction() == KeyEvent.ACTION_UP) {
                nativeIme(session, 4, "", 0, 0);
            }
            return super.onKeyPreIme(code, event);
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo info) {
            InputConnection connection = super.onCreateInputConnection(info);
            if (connection == null) return null;
            final long connectionSession = session;
            return new InputConnectionWrapper(connection, false) {
                @Override public boolean beginBatchEdit() {
                    if (connectionSession != session) return false;
                    depth++;
                    return super.beginBatchEdit();
                }
                @Override public boolean endBatchEdit() {
                    if (connectionSession != session) return false;
                    boolean result = super.endBatchEdit();
                    if (depth > 0) endEdit();
                    return result;
                }
                @Override public boolean setComposingText(CharSequence text, int cursor) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.setComposingText(text, cursor); }
                    finally { endEdit(); }
                }
                @Override public boolean setComposingRegion(int start, int end) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.setComposingRegion(start, end); }
                    finally { endEdit(); }
                }
                @Override public boolean finishComposingText() {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.finishComposingText(); }
                    finally { endEdit(); }
                }
                @Override public boolean commitText(CharSequence text, int cursor) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.commitText(text, cursor); }
                    finally { endEdit(); }
                }
                @Override public boolean setSelection(int start, int end) {
                    if (connectionSession != session) return false;
                    depth++;
                    try { return super.setSelection(start, end); }
                    finally { endEdit(); }
                }
                @Override public boolean deleteSurroundingText(int before, int after) {
                    if (connectionSession != session) return false;
                    if (getText().length() == 0 && !marked) {
                        nativeIme(session, 2, "", before, after);
                        return true;
                    }
                    depth++;
                    try { return super.deleteSurroundingText(before, after); }
                    finally { endEdit(); }
                }
                @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
                    if (connectionSession != session) return false;
                    if (getText().length() == 0 && !marked) {
                        nativeIme(session, 3, "", before, after);
                        return true;
                    }
                    depth++;
                    try { return super.deleteSurroundingTextInCodePoints(before, after); }
                    finally { endEdit(); }
                }
                @Override public boolean sendKeyEvent(KeyEvent event) {
                    if (connectionSession != session) return false;
                    if (event.getKeyCode() == KeyEvent.KEYCODE_DEL) {
                        if (event.getAction() == KeyEvent.ACTION_DOWN) deleteSurroundingText(1, 0);
                        return true;
                    }
                    if (event.getKeyCode() == KeyEvent.KEYCODE_ENTER) {
                        if (event.getAction() == KeyEvent.ACTION_DOWN) commitText("\n", 1);
                        return true;
                    }
                    return super.sendKeyEvent(event);
                }
                @Override public boolean performEditorAction(int action) {
                    if (connectionSession != session) return false;
                    if (action == EditorInfo.IME_ACTION_DONE) {
                        finishComposingText();
                        nativeIme(session, 4, "", 0, 0);
                        return true;
                    }
                    return commitText("\n", 1);
                }
            };
        }
    }

    // ── notifications ────────────────────────────────────────────────────────

    public void requestNotificationAuthorization() {
        runOnUiThread(() -> {
            if (Build.VERSION.SDK_INT >= 33
                    && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                            != PackageManager.PERMISSION_GRANTED
                    && !getSharedPreferences(NOTIFICATION_PERMISSION_PREFERENCES, MODE_PRIVATE)
                            .getBoolean(NOTIFICATION_PERMISSION_REQUESTED, false)) {
                getSharedPreferences(NOTIFICATION_PERMISSION_PREFERENCES, MODE_PRIVATE)
                        .edit()
                        .putBoolean(NOTIFICATION_PERMISSION_REQUESTED, true)
                        .apply();
                requestPermissions(
                        new String[] {Manifest.permission.POST_NOTIFICATIONS},
                        NOTIFICATION_PERMISSION_REQUEST);
            }
        });
    }

    public void showAgentNotification(
            String notificationId, String title, String body, String opaqueLocator) {
        runOnUiThread(() -> {
            Intent intent = new Intent(this, GpuiHostActivity.class)
                    .setAction("ai.vibex.mobile.OPEN_AGENT_NOTIFICATION")
                    .putExtra(EXTRA_NOTIFICATION_ID, notificationId)
                    .putExtra(EXTRA_NOTIFICATION_LOCATOR, opaqueLocator)
                    .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            int requestCode = notificationId.hashCode() & 0x7fffffff;
            PendingIntent pendingIntent = PendingIntent.getActivity(
                    this,
                    requestCode,
                    intent,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
            Notification notification = new Notification.Builder(this, AGENT_NOTIFICATION_CHANNEL)
                    .setSmallIcon(ai.vibex.mobile.R.drawable.ic_launcher_foreground)
                    .setContentTitle(title)
                    .setContentText(body)
                    .setCategory(Notification.CATEGORY_MESSAGE)
                    .setAutoCancel(true)
                    .setNumber(1)
                    .setContentIntent(pendingIntent)
                    .build();
            NotificationManager manager = getSystemService(NotificationManager.class);
            manager.notify(notificationId, 0, notification);
        });
    }

    private void createAgentNotificationChannel() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(
                AGENT_NOTIFICATION_CHANNEL,
                "Agent activity",
                NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("Agent approvals, input requests, and completed work");
        // A launcher badge is a property of the channel: the agent channel is
        // the one that stands for work waiting for the user, so it is the one
        // that is allowed to leave a mark on the icon.
        channel.setShowBadge(true);
        manager.createNotificationChannel(channel);
    }

    /**
     * Tells the user a newer Vibex is waiting.
     *
     * One tag and one id keep a discovered release to a single notification: a
     * later check replaces the previous one instead of stacking copies. The tap
     * only routes into the app — the settings card owns the actual download and
     * install, and no update payload travels through the notification.
     */
    public void showUpdateNotification(String version, String title, String body) {
        runOnUiThread(() -> {
            Intent intent = new Intent(this, GpuiHostActivity.class)
                    .setAction(ACTION_OPEN_APP_UPDATE)
                    .putExtra(EXTRA_NOTIFICATION_ID, version)
                    .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            PendingIntent pendingIntent = PendingIntent.getActivity(
                    this,
                    UPDATE_NOTIFICATION_ID,
                    intent,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
            Notification notification = new Notification.Builder(this, UPDATE_NOTIFICATION_CHANNEL)
                    .setSmallIcon(ai.vibex.mobile.R.drawable.ic_launcher_foreground)
                    .setContentTitle(title)
                    .setContentText(body)
                    .setCategory(Notification.CATEGORY_RECOMMENDATION)
                    .setAutoCancel(true)
                    .setOnlyAlertOnce(true)
                    .setContentIntent(pendingIntent)
                    .build();
            NotificationManager manager = getSystemService(NotificationManager.class);
            manager.notify(UPDATE_NOTIFICATION_TAG, UPDATE_NOTIFICATION_ID, notification);
        });
    }

    private void createUpdateNotificationChannel() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(
                UPDATE_NOTIFICATION_CHANNEL,
                "App updates",
                NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("New Vibex versions available for this device");
        manager.createNotificationChannel(channel);
    }

    private static void handleNotificationIntent(Intent intent) {
        if (intent == null) {
            return;
        }
        if (ACTION_OPEN_APP_UPDATE.equals(intent.getAction())) {
            intent.setAction(null);
            intent.removeExtra(EXTRA_NOTIFICATION_ID);
            nativeOnUpdateNotificationActivated();
            return;
        }
        String notificationId = intent.getStringExtra(EXTRA_NOTIFICATION_ID);
        String opaqueLocator = intent.getStringExtra(EXTRA_NOTIFICATION_LOCATOR);
        if (notificationId != null && !notificationId.isEmpty()
                && opaqueLocator != null && !opaqueLocator.isEmpty()) {
            intent.removeExtra(EXTRA_NOTIFICATION_ID);
            intent.removeExtra(EXTRA_NOTIFICATION_LOCATOR);
            nativeOnNotificationActivated(notificationId, opaqueLocator);
        }
    }

    // ── application updates ──────────────────────────────────────────────────

    /** The installed package version, which is what an update is compared against. */
    @SuppressWarnings("deprecation")
    public String appVersionName() {
        try {
            return getPackageManager().getPackageInfo(getPackageName(), 0).versionName;
        } catch (PackageManager.NameNotFoundException error) {
            return "";
        }
    }

    /** Opens an https page in whatever browser the device has. */
    public void openExternalUrl(String url) {
        runOnUiThread(() -> {
            Uri uri = Uri.parse(url);
            if (!"https".equals(uri.getScheme())) {
                return;
            }
            Intent intent = new Intent(Intent.ACTION_VIEW, uri)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                startActivity(intent);
            } catch (ActivityNotFoundException ignored) {
                // No browser to open the download page with.
            }
        });
    }

    /**
     * Hands a verified update APK to the system package installer.
     *
     * The APK is only ever the staged file the Rust updater verified against
     * the signed manifest, and this method re-checks that it lives inside the
     * app's own files directory before exposing it through the FileProvider.
     * Android itself refuses the install when the package is signed with a
     * different key than the installed app, so the download cannot replace this
     * app with anything the release key did not sign.
     */
    public void installUpdatePackage(String path) {
        runOnUiThread(() -> {
            File file;
            File filesDir;
            try {
                file = new File(path).getCanonicalFile();
                filesDir = getFilesDir().getCanonicalFile();
            } catch (Exception error) {
                return;
            }
            if (!file.isFile()
                    || !file.getPath().startsWith(filesDir.getPath() + File.separator)) {
                return;
            }
            Uri uri = FileProvider.getUriForFile(
                    this, getPackageName() + UPDATE_FILE_AUTHORITY_SUFFIX, file);
            Intent intent = new Intent(Intent.ACTION_VIEW)
                    .setDataAndType(uri, "application/vnd.android.package-archive")
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                startActivity(intent);
            } catch (ActivityNotFoundException ignored) {
                // No package installer to hand the APK to.
            }
        });
    }

    // ── background connection service ────────────────────────────────────────

    public void startRemoteConnectionService() {
        RemoteConnectionService.start(getApplicationContext());
    }

    public void stopRemoteConnectionService() {
        RemoteConnectionService.stop(getApplicationContext());
    }

    // ── battery allowlist ────────────────────────────────────────────────────

    /** Whether the system exempts this app from Doze/App Standby battery optimizations. */
    public boolean isIgnoringBatteryOptimizations() {
        PowerManager powerManager = getSystemService(PowerManager.class);
        return powerManager != null
                && powerManager.isIgnoringBatteryOptimizations(getPackageName());
    }

    /**
     * Opens the system dialog that asks the user to exempt the app from battery
     * optimizations, falling back to the app details page when unavailable.
     */
    public void requestIgnoreBatteryOptimizations() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                    .setData(Uri.parse("package:" + getPackageName()))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                startActivity(intent);
            } catch (ActivityNotFoundException error) {
                Intent appDetails = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
                        .setData(Uri.parse("package:" + getPackageName()))
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                try {
                    startActivity(appDetails);
                } catch (ActivityNotFoundException ignored) {
                    // Nothing else to open; the user can find the page manually.
                }
            }
        });
    }

    /**
     * Opens this app's page in the system settings.
     *
     * A denied local-network permission can only be re-granted there, and the
     * pairing screen that reports the denial has no other way to send the user
     * to it. A missing settings app is swallowed: the caller already explained
     * the problem, and there is nothing else the user could do from here.
     */
    public void openAppSettings() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
            intent.setData(Uri.parse("package:" + getPackageName()));
            try {
                startActivity(intent);
            } catch (ActivityNotFoundException ignored) {
                // No settings app to open.
            }
        });
    }

    // ── pairing ──────────────────────────────────────────────────────────────

    public void launchPairingQrScanner() {
        runOnUiThread(() ->
                startActivity(new Intent(this, PairingQrScannerActivity.class)));
    }

    // ── LAN discovery ────────────────────────────────────────────────────────

    public void startLanPairingDiscovery() {
        runOnUiThread(() -> {
            if (Build.VERSION.SDK_INT >= 33
                    && checkSelfPermission(Manifest.permission.NEARBY_WIFI_DEVICES)
                            != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(
                        new String[] {Manifest.permission.NEARBY_WIFI_DEVICES},
                        LOCAL_NETWORK_PERMISSION_REQUEST);
                return;
            }
            startLanPairingDiscoveryAfterPermission();
        });
    }

    public void stopLanPairingDiscovery() {
        runOnUiThread(this::stopLanPairingDiscoveryOnUiThread);
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != LOCAL_NETWORK_PERMISSION_REQUEST) {
            return;
        }
        if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            startLanPairingDiscoveryAfterPermission();
        } else {
            emitLanDiscoveryEvent("permission_denied", null, null);
        }
    }

    private void startLanPairingDiscoveryAfterPermission() {
        stopLanPairingDiscoveryOnUiThread();
        nsdManager = (NsdManager) getSystemService(Context.NSD_SERVICE);
        lanDiscoveryListener = new NsdManager.DiscoveryListener() {
            @Override
            public void onDiscoveryStarted(String serviceType) {}

            @Override
            public void onServiceFound(NsdServiceInfo serviceInfo) {
                String type = serviceInfo.getServiceType();
                if (!VIBEX_SERVICE_TYPE.equals(type) && !"_vibex._tcp".equals(type)) {
                    return;
                }
                resolveLanService(serviceInfo);
            }

            @Override
            public void onServiceLost(NsdServiceInfo serviceInfo) {
                emitLanDiscoveryEvent("removed", serviceInfo, null);
            }

            @Override
            public void onDiscoveryStopped(String serviceType) {}

            @Override
            public void onStartDiscoveryFailed(String serviceType, int errorCode) {
                emitLanDiscoveryEvent("failed", null, null);
                stopLanPairingDiscoveryOnUiThread();
            }

            @Override
            public void onStopDiscoveryFailed(String serviceType, int errorCode) {
                lanDiscoveryListener = null;
            }
        };
        try {
            nsdManager.discoverServices(
                    VIBEX_SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, lanDiscoveryListener);
        } catch (RuntimeException error) {
            emitLanDiscoveryEvent("failed", null, null);
            stopLanPairingDiscoveryOnUiThread();
        }
    }

    @SuppressWarnings("deprecation")
    private void resolveLanService(NsdServiceInfo serviceInfo) {
        String key = serviceInfo.getServiceName();
        if (pendingResolutions.containsKey(key)) {
            return;
        }
        NsdManager.ResolveListener listener = new NsdManager.ResolveListener() {
            @Override
            public void onResolveFailed(NsdServiceInfo failed, int errorCode) {
                pendingResolutions.remove(key);
            }

            @Override
            public void onServiceResolved(NsdServiceInfo resolved) {
                pendingResolutions.remove(key);
                emitLanDiscoveryEvent("candidate", resolved, resolved.getAttributes());
            }
        };
        pendingResolutions.put(key, listener);
        try {
            nsdManager.resolveService(serviceInfo, listener);
        } catch (RuntimeException error) {
            pendingResolutions.remove(key);
        }
    }

    private void stopLanPairingDiscoveryOnUiThread() {
        if (nsdManager != null && lanDiscoveryListener != null) {
            try {
                nsdManager.stopServiceDiscovery(lanDiscoveryListener);
            } catch (RuntimeException ignored) {
                // The listener may already have been stopped by Android.
            }
        }
        pendingResolutions.clear();
        lanDiscoveryListener = null;
        nsdManager = null;
    }

    private static void emitLanDiscoveryEvent(
            String kind, NsdServiceInfo service, Map<String, byte[]> attributes) {
        try {
            JSONObject event = new JSONObject();
            event.put("kind", kind);
            event.put("serviceInstance", service == null ? "" : service.getServiceName());
            event.put("port", service == null ? 0 : service.getPort());
            event.put("interfaceScope", "");
            event.put("host", resolvedNumericHost(service));
            JSONObject txt = new JSONObject();
            if (attributes != null) {
                for (Map.Entry<String, byte[]> entry : attributes.entrySet()) {
                    txt.put(entry.getKey(), new String(entry.getValue(), StandardCharsets.UTF_8));
                }
            }
            event.put("txt", txt);
            nativeOnLanDiscoveryEvent(event.toString());
        } catch (Exception ignored) {
            nativeOnLanDiscoveryEvent("{\"kind\":\"failed\"}");
        }
    }

    @SuppressWarnings("deprecation")
    private static String resolvedNumericHost(NsdServiceInfo service) {
        if (service == null) {
            return "";
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // Zero-config listeners are IPv4-only; Rust still accepts the
            // numeric fallback for Direct HTTPS candidates.
            String fallback = "";
            for (InetAddress address : service.getHostAddresses()) {
                if (address instanceof Inet4Address) {
                    return address.getHostAddress();
                }
                if (fallback.isEmpty()) {
                    fallback = address.getHostAddress();
                }
            }
            if (!fallback.isEmpty()) {
                return fallback;
            }
        }
        InetAddress address = service.getHost();
        return address == null ? "" : address.getHostAddress();
    }
}
