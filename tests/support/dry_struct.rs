//! `Dry::Struct` classes, lowered at ingest (`ingest::dry_struct`). One
//! contract for the interpreted and native lanes.

pub fn overlay() -> super::emit_and_run::Overlay {
    super::emit_and_run::real_blog()
        .write(
            "lib/shop/types.rb",
            "module Shop\n  module Types\n    include Dry.Types()\n\n    Loud = Types.Constructor(String) do |value|\n      next \"none\" if value.nil?\n\n      value.to_s.upcase\n    end\n  end\nend\n",
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
    CURRENCY = "RUB"
    CODE = ::Shop::Types::Coercible::String
    Upcased = ::Shop::Types.Constructor(String) { |value| value.to_s.upcase }

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
    attribute? :shared, ::Shop::Types::Array.default([], shared: true)
    attribute? :computed, ::Shop::Types::Integer.default(::Shop::Money.new(3).cents)
    attribute? :plan, ::Shop::Types::Hash.schema(tier: ::Shop::Types::String.default("basic"))
    attribute? :payer, ::Shop::Kinds::PAYER
    attribute? :code, CODE
    attribute? :shout, Upcased
    attribute? :list, ::Shop::Types::Array.constructor { |value| Array(value) }

  end
end
"#,
        )
        .write(
            "lib/shop/address.rb",
            "module Shop\n  class Address < Dry::Struct\n    attribute :city, ::Shop::Types::String\n  end\nend\n",
        )
        .write(
            "lib/shop/delivery.rb",
            "module Shop\n  class Delivery < Dry::Struct\n    attribute :to do\n      attributes_from Address\n    end\n    attribute? :raw, ::Shop::Types::JSON::Hash\n    attribute :notes?, ::Shop::Types::Array\n    attribute? :volume, ::Shop::Types::Loud\n  end\nend\n",
        )
        .write(
            "lib/shop/kinds.rb",
            "module Shop\n  module Kinds\n    PAYER = ::Shop::Types.Instance(::Shop::Money) | ::Shop::Types.Instance(::Shop::Refund)\n  end\nend\n",
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

/// What only the interpreted lane runs. With a full forwarder anywhere in
/// the app, every `X.new(**h)` has to prove its `initialize`: the
/// structs' must be found, `::` spelling included; Spinel refuses `...`
/// itself. And date and decimal coercions, which need stdlib Spinel lacks.
pub fn ruby_overlay() -> super::emit_and_run::Overlay {
    stamp_overlay(overlay()).write(
        "lib/shop/wrapper.rb",
        "module Shop\n  class Wrapper\n    def initialize(...)\n      setup(...)\n    end\n\n    def setup(*args, **kwargs)\n      @args = args\n    end\n  end\nend\n",
    )
}

/// A struct parsing dates and decimals.
pub fn stamp_overlay(base: super::emit_and_run::Overlay) -> super::emit_and_run::Overlay {
    base.write(
        "lib/shop/stamp.rb",
        r#"module Shop
  class Stamp < Dry::Struct
    attribute? :on, ::Shop::Types::Params::Date
    attribute? :at, ::Shop::Types::JSON::DateTime
    attribute? :seen, ::Shop::Types::Params::Time.optional
    attribute? :price, ::Shop::Types::Coercible::Decimal
    attribute? :fee, ::Shop::Types::Params::Decimal
  end
end
"#,
    )
}

pub const STAMP_ASSERTIONS: &str = r#"
s = Shop::Stamp.new(on: "2026-01-02", at: "2026-01-02T10:00:00+03:00", seen: nil, price: "1.25", fee: "2.5")
raise "date" unless s.on == Date.new(2026, 1, 2)
raise "date time" unless s.at.hour == 10 && s.at.is_a?(DateTime)
raise "optional time" unless s.seen.nil?
raise "coercible decimal" unless s.price == BigDecimal("1.25")
raise "params decimal" unless s.fee == BigDecimal("2.5")
raise "date passes through" unless Shop::Stamp.new(on: Date.new(2020, 5, 6)).on.month == 5
[{ on: "nope" }, { on: 5 }, { price: "x" }, { fee: "x" }].each do |bad|
  begin
    Shop::Stamp.new(bad)
    raise "stamp accepted #{bad.inspect}"
  rescue Dry::Struct::Error
  end
end
puts "dry-struct stamp contract passed"
"#;

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
a1 = client.order(base)
a2 = client.order(base)
raise "shared default" unless a1.shared == [] && a1.shared.equal?(a2.shared)
raise "computed default" unless a1.computed == 3
raise "schema key default" unless client.order(base.merge(plan: {})).plan == { tier: "basic" }
raise "sum left" unless client.order(base.merge(payer: Shop::Money.new(1))).payer.cents == 1
raise "sum right" unless client.order(base.merge(payer: r)).payer.id == "7"
begin
  client.order(base.merge(payer: "nobody"))
  raise "sum accepted a string"
rescue Dry::Struct::Error
end
raise "constant type" unless client.order(base.merge(code: 5)).code == "5"
raise "constructor" unless client.order(base.merge(shout: :hi)).shout == "HI"
raise "array constructor" unless client.order(base.merge(list: "x")).list == ["x"]
d = Shop::Delivery.new(to: { city: "Omsk" }, raw: { "a" => 1 }, volume: :hi)
raise "attributes_from" unless d.to.city == "Omsk" && d.to.is_a?(Shop::Delivery::To)
raise "json hash" unless d.raw == { "a" => 1 }
raise "name? is omittable" unless d.notes.nil? && Shop::Delivery.new(to: { city: "x" }, notes: [1]).notes == [1]
raise "types constant constructor" unless d.volume == "HI"
raise "constructor next" unless Shop::Delivery.new(to: { city: "x" }, volume: nil).volume == "none"
begin
  Shop::Delivery.new(to: { city: "x" }, raw: 1)
  raise "json hash accepted a number"
rescue Dry::Struct::Error
end
begin
  Shop::Delivery.new(to: {})
  raise "copied required attribute missing accepted"
rescue Dry::Struct::Error
end
puts "dry-struct contract passed"
"#;
