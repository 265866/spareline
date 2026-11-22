package spareline;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Typeface;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.Deque;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

// Draws the lock screen notice at the display's size and sets it as user 0's
// lock screen wallpaper only, under NAME, which `dumpsys wallpaper` prints as
// the lock wallpaper's mName. It touches nothing but that wallpaper: no radio,
// data, or other setting (see AGENTS.md). Runs as the adb shell user:
//
//   CLASSPATH=<dex> app_process / spareline.LockScreen NAME BASE64_TEXT
//
// BASE64_TEXT is UTF-8: the title, then one body paragraph per line. The shell
// user holds SET_WALLPAPER; there is no app Context, so the wallpaper service
// is called through its binder interface.
public final class LockScreen {
    private static final int FLAG_LOCK = 2;
    private static final int USER = 0;
    private static final String SHELL_PACKAGE = "com.android.shell";

    public static void main(String[] args) {
        // Like scrcpy-server: each run is pushed fresh, so nothing stays behind.
        new File(System.getProperty("java.class.path")).delete();
        try {
            String[] lines = new String(Base64.getDecoder().decode(args[1]), StandardCharsets.UTF_8).split("\n");
            int[] size = displaySize();
            setLockWallpaper(args[0], render(size[0], size[1], lines));
            System.out.println("lock screen wallpaper set: " + args[0]);
        } catch (Throwable error) {
            error.printStackTrace();
            System.exit(1);
        }
    }

    // Portrait width and height in pixels. Resources.getSystem() reports 0x0
    // outside an app, so this asks the display manager.
    private static int[] displaySize() throws Exception {
        Object displays = Class.forName("android.hardware.display.DisplayManagerGlobal").getMethod("getInstance").invoke(null);
        Object info = displays.getClass().getMethod("getDisplayInfo", int.class).invoke(displays, 0);
        int width = info.getClass().getField("logicalWidth").getInt(info);
        int height = info.getClass().getField("logicalHeight").getInt(info);
        return new int[] {Math.min(width, height), Math.max(width, height)};
    }

    // The text sits between the clock across the middle of the lock screen and
    // the unlock hint at the bottom (measured on the NUU B30). It shrinks until
    // it fits there; text that does not fit even at the smallest size is
    // refused before the wallpaper is touched.
    private static byte[] render(int width, int height, String[] lines) throws Exception {
        // Zygote loads the system fonts for apps; app_process has to do it.
        if (Typeface.DEFAULT == null) Typeface.class.getMethod("loadPreinstalledSystemFontMap").invoke(null);
        Bitmap bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        canvas.drawColor(0xFF0C0D0F);
        float center = width / 2f;
        float textWidth = width * 0.88f;

        Paint title = textPaint(Typeface.create(Typeface.DEFAULT, Typeface.BOLD), 0xFFF0F0F2, width * 0.07f);
        while (title.measureText(lines[0]) > textWidth) title.setTextSize(title.getTextSize() * 0.95f);
        float y = height * 0.717f - title.ascent();
        canvas.drawText(lines[0], center, y, title);

        y += title.descent() + width * 0.035f;
        Paint divider = new Paint(Paint.ANTI_ALIAS_FLAG);
        divider.setColor(0xFF5A5C62);
        divider.setStrokeWidth(Math.max(2f, width * 0.0028f));
        canvas.drawLine(center - width * 0.055f, y, center + width * 0.055f, y, divider);

        y += width * 0.045f;
        float room = height * 0.86f - y;
        Paint body = textPaint(Typeface.DEFAULT, 0xFFA5A8AF, width * 0.039f);
        List<String> wrapped = wrap(lines, body, textWidth);
        while (!fits(wrapped, body, textWidth, room)) {
            if (body.getTextSize() * 0.95f < width * 0.02f)
                throw new IllegalArgumentException("the notice does not fit on this screen; shorten its lines");
            body.setTextSize(body.getTextSize() * 0.95f);
            wrapped = wrap(lines, body, textWidth);
        }
        for (String line : wrapped) {
            y -= body.ascent();
            canvas.drawText(line, center, y, body);
            y += body.descent() + gap(body);
        }

        ByteArrayOutputStream png = new ByteArrayOutputStream();
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, png);
        return png.toByteArray();
    }

    private static Paint textPaint(Typeface typeface, int color, float size) {
        Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        paint.setTypeface(typeface);
        paint.setColor(color);
        paint.setTextAlign(Paint.Align.CENTER);
        paint.setTextSize(size);
        return paint;
    }

    // The body paragraphs (lines[1..]) broken at spaces into lines no wider
    // than maxWidth where a word allows it.
    private static List<String> wrap(String[] lines, Paint paint, float maxWidth) {
        List<String> wrapped = new ArrayList<>();
        for (int i = 1; i < lines.length; i++) {
            String line = "";
            for (String word : lines[i].split(" ")) {
                String longer = line.isEmpty() ? word : line + " " + word;
                if (!line.isEmpty() && paint.measureText(longer) > maxWidth) {
                    wrapped.add(line);
                    line = word;
                } else {
                    line = longer;
                }
            }
            if (!line.isEmpty()) wrapped.add(line);
        }
        return wrapped;
    }

    private static float gap(Paint paint) {
        return paint.getTextSize() * 0.3f;
    }

    private static boolean fits(List<String> lines, Paint paint, float maxWidth, float room) {
        for (String line : lines)
            if (paint.measureText(line) > maxWidth) return false;
        return lines.size() * (paint.descent() - paint.ascent() + gap(paint)) <= room;
    }

    // The service names the wallpaper as soon as it hands out the file, before
    // the image arrives, so a run that fails after that clears the lock
    // wallpaper: the check then reads the home screen's wallpaper instead of
    // a name with no notice behind it.
    private static void setLockWallpaper(String name, byte[] png) throws Exception {
        IBinder service = (IBinder) Class.forName("android.os.ServiceManager").getMethod("getService", String.class).invoke(null, "wallpaper");
        Object wallpapers = Class.forName("android.app.IWallpaperManager$Stub").getMethod("asInterface", IBinder.class).invoke(null, service);
        CountDownLatch stored = new CountDownLatch(1);
        Binder completion = new Binder() {
            @Override
            protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
                if (code < FIRST_CALL_TRANSACTION || code > LAST_CALL_TRANSACTION) return false;
                stored.countDown();
                return true;
            }
        };
        Class<?> callbackType = Class.forName("android.app.IWallpaperManagerCallback");
        Object callback = Class.forName("android.app.IWallpaperManagerCallback$Stub").getMethod("asInterface", IBinder.class).invoke(null, completion);

        ParcelFileDescriptor file = (ParcelFileDescriptor) call(wallpapers, "setWallpaper",
                new String[] {name, SHELL_PACKAGE}, new int[] {FLAG_LOCK, USER}, callbackType, callback);
        if (file == null) throw new IllegalStateException("the wallpaper service refused the lock screen wallpaper");
        try {
            try (FileOutputStream out = new ParcelFileDescriptor.AutoCloseOutputStream(file)) {
                out.write(png);
            }
            if (!stored.await(15, TimeUnit.SECONDS))
                throw new IllegalStateException("the wallpaper service did not confirm the new lock screen wallpaper");
        } catch (Throwable error) {
            try {
                call(wallpapers, "clearWallpaper", new String[] {SHELL_PACKAGE}, new int[] {FLAG_LOCK, USER}, null, null);
            } catch (Throwable clearError) {
                error.addSuppressed(clearError);
            }
            throw error;
        }
    }

    // IWallpaperManager's parameters differ between Android versions, so they
    // are filled by type: the strings and ints in order (for both methods the
    // ints are `which` and the user id), booleans false, a new Bundle, the
    // completion callback, and null for any other object (Android 14's crop
    // hint, Android 15's crop lists).
    private static Object call(Object target, String name, String[] strings, int[] ints, Class<?> callbackType, Object callback) throws Exception {
        Method method = null;
        for (Method candidate : target.getClass().getMethods())
            if (candidate.getName().equals(name)) method = candidate;
        if (method == null) throw new IllegalStateException("IWallpaperManager has no " + name);
        Deque<String> nextStrings = new ArrayDeque<>(Arrays.asList(strings));
        Deque<Integer> nextInts = new ArrayDeque<>();
        for (int value : ints) nextInts.add(value);
        Class<?>[] types = method.getParameterTypes();
        Object[] values = new Object[types.length];
        for (int i = 0; i < types.length; i++) {
            if (types[i] == String.class && !nextStrings.isEmpty()) values[i] = nextStrings.poll();
            else if (types[i] == int.class && !nextInts.isEmpty()) values[i] = nextInts.poll();
            else if (types[i] == boolean.class) values[i] = false;
            else if (types[i] == Bundle.class) values[i] = new Bundle();
            else if (types[i] == callbackType) values[i] = callback;
            else if (types[i].isPrimitive()) throw new IllegalStateException("unexpected parameters: " + method);
        }
        if (!nextStrings.isEmpty() || !nextInts.isEmpty()) throw new IllegalStateException("unexpected parameters: " + method);
        return method.invoke(target, values);
    }
}
