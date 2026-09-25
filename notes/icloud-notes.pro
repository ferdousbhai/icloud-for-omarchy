QT += core gui qml quick quickcontrols2 printsupport sql

CONFIG += c++17 release
TARGET = icloud-notes
TEMPLATE = app

HEADERS += \
    src/notesbackend.h \
    src/markdownhighlighter.h \
    src/signin.h

SOURCES += \
    src/main.cpp \
    src/notesbackend.cpp \
    src/markdownhighlighter.cpp

RESOURCES += qml/resources.qrc
