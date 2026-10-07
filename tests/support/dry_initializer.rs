//! `Dry::Initializer` classes, lowered at ingest (`ingest::dry_initializer`).
//! One contract for the interpreted and native lanes, checked against
//! dry-initializer 3.2 with dry-types 1.8.

// Each lane uses only its part.
#![allow(dead_code)]

pub fn overlay() -> super::emit_and_run::Overlay {
    super::emit_and_run::real_blog()
        .write(
            "lib/svc/types.rb",
            r##"module Svc
  module Types
    include Dry.Types()
  end
end
"##,
        )
        .write(
            "lib/svc/charge.rb",
            r##"module Svc
  class Base
    extend Dry::Initializer

    option :currency, ::Svc::Types::Strict::String, default: -> { default_currency }

    def self.call(**options)
      new(**options).call
    end

    private

    def default_currency
      "RUB"
    end
  end

  class Charge < Base
    param :order_id, ::Svc::Types::Coercible::Integer
    option :amount, ::Svc::Types::Coercible::Integer
    option :note, ::Svc::Types::Coercible::String.optional, optional: true
    option :paid, ::Svc::Types::Strict::Bool, default: -> { false }
    option :tags, ::Svc::Types::Array.of(::Svc::Types::Coercible::Symbol), default: -> { [] }
    option :title, type: proc { |value| value.to_s.upcase }, optional: true
    option :secret, ::Svc::Types::Strict::String, reader: :private, optional: true
    option :kind, as: :category, optional: true

    def call
      "#{order_id}:#{amount}:#{currency}"
    end

    def revealed
      secret
    end

    def note_given?
      @note != Dry::Initializer::UNDEFINED
    end
  end

  class Quote < Base
    option :amount, ::Svc::Types::Coercible::Integer

    def call
      "#{amount} #{currency}"
    end
  end

  class Refund < Charge
    option :reason, ::Svc::Types::Strict::String

    def initialize(order_id, **options)
      super
      @reason = "#{reason}!"
    end

    def call
      "refund #{reason}"
    end
  end
end
"##,
        )
}

/// With a full forwarder anywhere in the app, `new(**options)` has to
/// prove the lowered `initialize`; Spinel refuses `...` itself.
pub fn ruby_overlay() -> super::emit_and_run::Overlay {
    overlay().write(
        "lib/svc/wrapper.rb",
        "module Svc\n  class Wrapper\n    def initialize(...)\n      setup(...)\n    end\n\n    def setup(*args, **kwargs)\n      @args = args\n    end\n  end\nend\n",
    )
}

pub const ASSERTIONS: &str = r##"
c = Svc::Charge.new("7", amount: "12", note: 5, tags: ["a", :b], title: "x", secret: "s", kind: "k", stray: 1)
raise "param" unless c.order_id == 7
raise "coercible integer" unless c.amount == 12
raise "coercible optional" unless c.note == "5"
raise "default" unless c.paid == false && c.currency == "RUB"
raise "array of" unless c.tags == [:a, :b]
raise "proc type" unless c.title == "X"
raise "private reader" if c.respond_to?(:secret)
raise "private reader value" unless c.revealed == "s"
raise "as" unless c.category == "k"
d = Svc::Charge.new(1, amount: 2, currency: "USD", paid: true)
raise "optional nil" unless d.note.nil? && d.title.nil? && d.category.nil?
raise "left out" if d.note_given?
raise "given nil" unless Svc::Charge.new(1, amount: 1, note: nil).note_given?
raise "given over default" unless d.currency == "USD" && d.paid == true
raise "class call" unless Svc::Quote.call(amount: "3") == "3 RUB"
raise "subclass" unless Svc::Refund.new(1, amount: 1, reason: "r").call == "refund r!"
begin
  Svc::Charge.new(1)
  raise "missing option accepted"
rescue KeyError => e
  raise "message #{e.message}" unless e.message == "Svc::Charge: option 'amount' is required"
end
begin
  Svc::Charge.new(1, amount: "twelve")
  raise "coercible integer accepted a word"
rescue Dry::Types::CoercionError
end
begin
  Svc::Charge.new(1, amount: 1, paid: "yes")
  raise "strict bool accepted a string"
rescue Dry::Types::ConstraintError
end
begin
  Svc::Refund.new(1, amount: 1)
  raise "subclass option missing"
rescue KeyError
end

puts "dry-initializer contract passed"
"##;
