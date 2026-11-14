package sample;
public final class Collision {
    public static final class Date { public int value = 17; }
    public int base;
    public Collision() { base = 3; }
    public Collision(int value) { base = value; }
    public int same(int value) { return base + value; }
    public String same(String value) { return value + base; }
    public long external(java.util.Date date) { return date.getTime() + new Date().value; }
    public static void main(String[] args) {
        Collision c = new Collision(9);
        if (c.same(4) != 13 || !c.same("x").equals("x9") || c.external(new java.util.Date(100)) != 117) throw new AssertionError();
        System.out.println("PASS constructors overloads external type collision");
    }
}
