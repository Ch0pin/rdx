package demo;

import android.app.Activity;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/** Static-only fixture: two independent field receivers and a factory receiver. */
public class Screen extends Activity {
    WebView help;
    WebView checkout;
    public void configure() {
        help = new WebView(this);
        checkout = new WebView(this);
        help.setWebViewClient(new HelpClient());
        help.loadUrl("https://help.example.test/");
        checkout.setWebViewClient(new CheckoutClient());
        checkout.loadUrl("https://checkout.example.test/");
    }
    public void refresh() {
        help.evaluateJavascript("document.title", null);
    }
    public static WebView makeEmbedded(Activity host) {
        WebView embedded = new WebView(host);
        embedded.setWebViewClient(new EmbeddedClient());
        embedded.loadUrl("https://embedded.example.test/");
        return embedded;
    }
    public void route(WebView dynamic, String url) {
        dynamic.loadUrl(url);
    }
    static class HelpClient extends WebViewClient {}
    static class CheckoutClient extends WebViewClient {}
    static class EmbeddedClient extends WebViewClient {}
}
