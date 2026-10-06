//! `Dry::Struct` classes, lowered at ingest (`ingest::dry_struct`). One
//! contract for the interpreted and native lanes.

pub fn overlay() -> super::emit_and_run::Overlay {
    super::emit_and_run::real_blog()
        .write(
            "lib/shop/types.rb",
            "module Shop\n  module Types\n    include Dry.Types()\n  end\nend\n",
        )
        .write(
            "lib/shop/base_response.rb",
            r#"module Shop
  class BaseResponse < Dry::Struct
    transform_keys(&:to_sym)

    attribute? :response, ::Shop::Types::Hash
  end
end
"#,
        )
        .write(
            "lib/shop/refund.rb",
            r#"module Shop
  class Refund < BaseResponse
    attribute :id, ::Shop::Types::Coercible::String
    attribute :amount, ::Shop::Types::Coercible::Integer
    attribute :paid, ::Shop::Types::Strict::Bool
    attribute? :note, ::Shop::Types::Strict::String.optional
    attribute? :status, ::Shop::Types::Coercible::String.default("new")
    attribute? :tags, ::Shop::Types::Array.of(::Shop::Types::Coercible::String)
  end
end
"#,
        )
        .write(
            "lib/shop/money.rb",
            "module Shop\n  class Money\n    def initialize(cents)\n      @cents = cents\n    end\n\n    def cents\n      @cents\n    end\n  end\nend\n",
        )
        .write(
            "lib/shop/order.rb",
            r#"module Shop
  class Order < BaseResponse
    attribute :amount do
      attribute :value, ::Shop::Types::Coercible::String
    end
    attribute :items, ::Shop::Types::Array do
      attribute :sku, ::Shop::Types::Strict::String
    end
    attribute? :refund, Refund
    attribute? :price, ::Shop::Types.Instance(::Shop::Money)
    attribute? :kind, ::Shop::Types::Coercible::Symbol
    attribute? :meta, ::Shop::Types::Hash.default({}.freeze)
    attribute? :state, ::Shop::Types::Coercible::String.enum("open", "closed")
    attribute? :qty, ::Shop::Types::Params::Integer
    attribute? :gift, ::Shop::Types::Params::Bool
    attribute? :currency, ::Shop::Types::Coercible::String.default(CURRENCY)
    attribute? :rush, ::Shop::Types::Strict::Bool.default { false }
    attribute? :rows, ::Shop::Types::Array.of(
      ::Shop::Types::Hash.schema(amount: ::Shop::Types::Coercible::Float, note?: ::Shop::Types::Coercible::String)
    )
    attribute? :source, ::Shop::Types::Coercible::String.default("WEB").enum("WEB", "APP")
    attribute? :at, ::Shop::Types::Strict::Time
    attribute? :extra, ::Shop::Types::Coercible::Hash
    attribute? :lines, ::Shop::Types::Array.of(
      ::Shop::Types::Strict::Hash.schema(sku: ::Shop::Types::Strict::String.meta(omittable: true))
    )
    attribute? :token, ::Shop::Types::Coercible::String.default { Shop::Money.new(7).cents.to_s }
    attribute? :label, ::Shop::Types::String
    attribute? :codes, ::Shop::Types::Array.of(::Shop::Types::String)
    attribute? :anything, ::Shop::Types::Any
    attribute? :loose, ::Shop::Types::Nominal::String

    CURRENCY = "RUB"
  end
end
"#,
        )
        .write(
            "lib/shop/client.rb",
            r#"module Shop
  class Client
    def refund(payload)
      Refund.new(**payload)
    end

    def qualified_refund(payload)
      ::Shop::Refund.new(**payload)
    end

    def parse(body)
      Refund.new(body)
    end

    def order(body)
      Order.new(body)
    end
  end
end
"#,
        )
}

/// With a full forwarder anywhere in the app, every `X.new(**h)` has to
/// prove its `initialize`: the structs' must be found, `::` spelling
/// included. Interpreted lane only: Spinel refuses `...` itself.
pub fn forwarding_overlay() -> super::emit_and_run::Overlay {
    overlay().write(
        "lib/shop/wrapper.rb",
        "module Shop\n  class Wrapper\n    def initialize(...)\n      setup(...)\n    end\n\n    def setup(*args, **kwargs)\n      @args = args\n    end\n  end\nend\n",
    )
}

pub const ASSERTIONS: &str = r#"
client = Shop::Client.new
r = client.refund(id: 7, amount: "12", paid: true, tags: [1, :b])
raise "coercible string" unless r.id == "7"
raise "qualified" unless client.qualified_refund(id: 1, amount: 2, paid: false).amount == 2
raise "coercible integer" unless r.amount == 12
raise "strict bool" unless r.paid == true
raise "omitted optional" unless r.note.nil?
raise "default" unless r.status == "new"
raise "array of" unless r.tags == ["1", "b"]
raise "inherited omitted" unless r.response.nil?
s = client.parse({ "id" => "x", "amount" => 3, "paid" => false, "note" => nil, "response" => { "a" => 1 } })
raise "string keys" unless s.id == "x" && s.amount == 3 && s.paid == false
raise "explicit nil" unless s.note.nil?
raise "inherited" unless s.response == { "a" => 1 }
begin
  client.refund(amount: 1, paid: true)
  raise "missing key accepted"
rescue Dry::Struct::Error
end
begin
  client.refund(id: 1, amount: 1, paid: "yes")
  raise "strict bool accepted a string"
rescue Dry::Struct::Error
end
begin
  client.refund(id: 1, amount: "twelve", paid: true)
  raise "coercible integer accepted a word"
rescue Dry::Struct::Error
end
o = client.order({ "amount" => { "value" => 5 }, "items" => [{ "sku" => "a" }, { "sku" => "b" }],
                   "refund" => r, "price" => Shop::Money.new(9), "kind" => "fast" })
raise "nested" unless o.amount.is_a?(Shop::Order::Amount) && o.amount.value == "5"
raise "array of nested" unless o.items.map(&:sku) == ["a", "b"] && o.items.first.is_a?(Shop::Order::Item)
raise "struct instance passes" unless o.refund.equal?(r)
raise "instance" unless o.price.cents == 9
raise "symbol" unless o.kind == :fast
raise "frozen default" unless o.meta == {}
raise "constant default" unless o.currency == "RUB"
raise "block default" unless o.rush == false
raise "default then enum" unless o.source == "WEB"
raise "computed default" unless o.token == "7"
# Under `Dry.Types()` a bare name is strict; `Any` and `Nominal::` are not.
base = { amount: { value: 1 }, items: [] }
raise "bare strict ok" unless client.order(base.merge(label: "x", codes: ["a"])).label == "x"
raise "any" unless client.order(base.merge(anything: 5)).anything == 5
raise "nominal" unless client.order(base.merge(loose: 5)).loose == 5
[{ label: 1 }, { codes: [1] }, { codes: "a" }].each do |bad|
  begin
    client.order(base.merge(bad))
    raise "bare name accepted #{bad.inspect}"
  rescue Dry::Struct::Error
  end
end
now = Time.now
t = client.order({ amount: { value: 1 }, items: [], at: now, extra: nil, lines: [{}, { sku: "s" }] })
raise "strict time" unless t.at == now
raise "coercible hash" unless t.extra == {}
raise "meta omittable" unless t.lines == [{}, { sku: "s" }]
begin
  client.order({ amount: { value: 1 }, items: [], at: "2026-01-01" })
  raise "strict time accepted a string"
rescue Dry::Struct::Error
end
rows = client.order({ amount: { value: 1 }, items: [], rows: [{ amount: "1.5", extra: 1 }, { amount: 2, note: 5 }] }).rows
raise "hash schema #{rows.inspect}" unless rows == [{ amount: 1.5 }, { amount: 2.0, note: "5" }]
begin
  client.order({ amount: { value: 1 }, items: [], rows: [{ "amount" => 2 }] })
  raise "hash schema took a string key"
rescue Dry::Struct::Error
end
p2 = client.order({ amount: { value: 1 }, items: [], state: :open, qty: "042", gift: "yes" })
raise "enum" unless p2.state == "open"
raise "params integer" unless p2.qty == 42
raise "params bool" unless p2.gift == true && client.order({ amount: { value: 1 }, items: [], gift: "0" }).gift == false
begin
  client.order({ amount: { value: 1 }, items: [], state: "lost" })
  raise "enum accepted an outsider"
rescue Dry::Struct::Error
end
begin
  client.order({ amount: { value: 1 }, items: [], gift: "maybe" })
  raise "params bool accepted maybe"
rescue Dry::Struct::Error
end
built = client.order({ amount: { value: 1 }, items: [], refund: { id: 3, amount: 4, paid: true } })
raise "struct from hash" unless built.refund.is_a?(Shop::Refund) && built.refund.amount == 4
begin
  client.order({ amount: 5, items: [] })
  raise "nested accepted a number"
rescue Dry::Struct::Error
end
begin
  client.order({ amount: { value: 1 }, items: [], price: 9 })
  raise "instance accepted a number"
rescue Dry::Struct::Error
end
puts "dry-struct contract passed"
"#;
